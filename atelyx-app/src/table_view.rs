//! 多维表格视图：canvas 自绘主体（网格 / 文本 / 样式 / 图片 / 选中框 / 把手 / 虚拟窗口）
//! + 顶部工具条 + 编辑器浮层 + 格式气泡 + 灯箱。模型真源 = core `TableFile`；
//! 模型变换走 `table_ops` 纯函数，消息处理与保存链在 `table_actions`。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use iced::widget::canvas::{self, Canvas, LineCap, LineDash, LineJoin, Path, Stroke};
use iced::widget::text_editor;
use iced::widget::{column, container, mouse_area, row, space, text};
use iced::{alignment, Color, Element, Length, Point, Rectangle, Size, Vector};

use crate::ui::icon::polylines;
use crate::ui::palette as pal;
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
const CELL_FONT: f32 = pal::FS_UI;
const NUM_FONT: f32 = pal::FS_CAPTION;
const LINE_FACTOR: f32 = 1.45;
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
/// 格式气泡与锚点的垂直间距（px）。
const BAR_GAP: f32 = 34.0;
/// 灯箱图片四周遮罩边距（px；点击遮罩关闭的命中区）。
const LIGHTBOX_MARGIN: f32 = 48.0;

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

        let mut row_h = Vec::with_capacity(data.rows.len());
        let mut row_y = Vec::with_capacity(data.rows.len() + 1);
        let mut y = 0.0f32;
        for row in &data.rows {
            row_y.push(y);
            let h = if let Some(h) = row.height {
                // 磁盘存量可能小于下限（写入侧钳制不覆盖旧数据）：读取侧同样钳到把手命中区下限
                (h as f32).max(table_ops::MIN_ROW_HEIGHT as f32) * zoom
            } else {
                let mut content = 0.0f32;
                for (i, f) in data.fields.iter().enumerate() {
                    content = content.max(cell_content_height(f, row, row.values.get(&f.id), col_w[i], zoom));
                }
                (content + 2.0 * CELL_PAD_Y * zoom).max(32.0 * zoom)
            };
            row_h.push(h);
            y += h;
        }
        row_y.push(y);
        self.view.layout = Some(LayoutCache {
            version: self.version,
            zoom,
            col_w,
            col_x,
            total_w,
            row_h,
            row_y,
            content_h: y + NEW_ROW_H * zoom,
        });
    }
}

#[derive(Default)]
pub struct TableViewState {
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
    pub lightbox: Option<Lightbox>,
    /// 上次按压（双击判定）。
    pub last_press: Option<(Instant, Point)>,
    /// 画布尺寸（Press/Context 播种；Move/Release 期间的命中几何用）。
    pub canvas_size: Size,
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
    pub handle: iced::widget::image::Handle,
    /// 原始像素尺寸（载入时经 image crate 读得；contain 缩放用）。
    pub size: (f32, f32),
}

#[derive(Clone)]
pub struct Lightbox {
    pub row_id: String,
    pub field_id: String,
    pub index: usize,
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
}

// ===== 消息 =====

/// 表格视图消息（部件上报 + 菜单/命令 + 保存链回执）。
#[derive(Debug, Clone)]
pub enum TableMsg {
    /// 画布内左键按下（画布局部坐标；origin = 画布在窗口中的原点，供弹层锚定单元格）。
    Press { window: iced::window::Id, pos: Point, size: Size, origin: Point },
    /// 画布内右键按下（上下文菜单）。
    Context { window: iced::window::Id, pos: Point, size: Size },
    /// 画布内鼠标移动（携带画布尺寸：首帧悬停即播种 canvas_size，供可见区图片加载）。
    Move { pos: Point, size: Size },
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
    ImageLoaded { rel: String, fp: (u128, u64), result: Result<Vec<u8>, String> },
    ImagesWritten { row_id: String, field_id: String, rels: Vec<String>, failed: usize },
    LightboxClose,
    LightboxNav(i32),
    LightboxDownload,
    /// 打开灯箱右键菜单（下载图片）。
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
    /// 打开历史侧栏（表格上下文）。
    OpenHistory,
    /// 无操作（吞键位）。
    Noop,
    /// 表格弹层内联输入草稿（改名 / 插列 / 添加字段共用）。
    PopupDraft(String),
    // 载入与保存
    Loaded(u64, String, Result<TableFile, String>),
    TableSaved(u64, String, SaveReason, Result<(), String>, TableFile),
    PasteText(String),
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

/// 单元格内容自然高度（不含上下内衬；image 区自带高度）。字号口径与绘制一致：
/// 基准字号 × zoom 再叠加单元格样式字号。
fn cell_content_height(
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
    let wrap_w = (col_w - 2.0 * CELL_PAD_X * zoom).max(8.0);
    wrap_lines(&text, size, wrap_w) as f32 * size * LINE_FACTOR
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

// ===== 文本估算（复用 chrome 估宽的单字符口径，行高/换行/截断同源）=====

fn text_width(text: &str, size: f32) -> f32 {
    text.chars().map(|c| crate::layout_view::estimate_char_w(c, size)).sum()
}

/// 按字符单位贪心换行的总行数（行高与绘制同源）。
fn wrap_lines(text: &str, size: f32, max_w: f32) -> usize {
    text.split('\n')
        .map(|line| {
            let mut lines = 1;
            let mut w = 0.0f32;
            for ch in line.chars() {
                let cw = crate::layout_view::estimate_char_w(ch, size);
                if w + cw > max_w && w > 0.0 {
                    lines += 1;
                    w = cw;
                } else {
                    w += cw;
                }
            }
            lines
        })
        .sum()
}

/// 逐行拆分（绘制用）：贪心按字符断行。
fn wrap_lines_text(text: &str, size: f32, max_w: f32) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let mut cur = String::new();
        let mut w = 0.0f32;
        for ch in line.chars() {
            let cw = crate::layout_view::estimate_char_w(ch, size);
            if w + cw > max_w && w > 0.0 {
                out.push(std::mem::take(&mut cur));
                w = 0.0;
            }
            cur.push(ch);
            w += cw;
        }
        out.push(cur);
    }
    out
}

/// 字段基准字号：数字 / 时长列小一档。
fn base_font_size(field: &TableField) -> f32 {
    match field.field_type.as_str() {
        "number" | "duration" => NUM_FONT,
        _ => CELL_FONT,
    }
}

/// 样式字体映射：随应用 IBM Plex Sans（default/sans）、系统通用族（serif/mono）、
/// 中文预设按系统字体名解析（缺字体回落默认，跨平台不保证）。
pub fn style_font(st: &CellStyle) -> iced::Font {
    let base = iced::Font { family: iced::font::Family::Name(crate::ui::FONT_FAMILY), ..iced::Font::default() };
    match st.font.as_deref() {
        Some("serif") => iced::Font { family: iced::font::Family::Serif, ..base },
        Some("mono") => iced::Font { family: iced::font::Family::Monospace, ..base },
        Some("hei") => named("Microsoft YaHei"),
        Some("song") => named("SimSun"),
        Some("kai") => named("KaiTi"),
        Some("fang") => named("FangSong"),
        _ => base,
    }
}

fn named(name: &'static str) -> iced::Font {
    iced::Font { family: iced::font::Family::Name(name), ..iced::Font::default() }
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
    pub thumbs: Vec<(String, Rectangle)>,
    pub tiles: Vec<(String, Rectangle)>,
    pub mode_btn: Option<Rectangle>,
    pub add_btn: Option<Rectangle>,
    pub remove_btn: Option<Rectangle>,
    /// 空值的整格添加热区。
    pub empty_add: Option<Rectangle>,
}

pub fn image_layout_of(cell: Rectangle, v: &serde_json::Value, zoom: f32) -> ImageLayout {
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
    if table_ops::image_display(v) == Some("grid") {
        let cols = grid_cols(cell.width, zoom, images.len());
        let tile_w = grid_tile_w(cell.width, zoom, cols);
        let tile_h = tile_w.min(IMAGE_GRID_SINGLE_MAX * zoom);
        let mut tiles = Vec::new();
        for (i, img) in images.iter().enumerate() {
            let r = i / cols;
            let c = i % cols;
            tiles.push((
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
            mode_btn: Some(Rectangle::new(inner.position(), Size::new(btn, btn))),
            add_btn: Some(Rectangle::new(
                Point::new(inner.x + btn + IMAGE_GAP * zoom, inner.y),
                Size::new(btn, btn),
            )),
            remove_btn: None,
            empty_add: None,
        }
    } else {
        let queue_h = if images.len() > 1 { IMAGE_QUEUE_H * zoom } else { 0.0 };
        let main = Rectangle::new(inner.position(), Size::new(inner.width, (inner.height - queue_h).max(0.0)));
        // 队列条仅多图时存在（单图 = 主图区铺满，无缩略图行）；
        // 数量超出格宽时截断（canvas 无逐格裁剪，画出去会盖到邻格，命中也落邻格）
        let mut thumbs = Vec::new();
        if images.len() > 1 {
            let thumb_step = (IMAGE_THUMB + IMAGE_GAP) * zoom;
            let max_thumbs = (((inner.width - 4.0 * zoom) / thumb_step).floor() as usize).max(1);
            for (i, img) in images.iter().enumerate().take(max_thumbs) {
                thumbs.push((
                    img.to_string(),
                    Rectangle::new(
                        Point::new(
                            inner.x + 2.0 * zoom + i as f32 * (IMAGE_THUMB + IMAGE_GAP) * zoom,
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
            mode_btn: (images.len() > 1).then(|| Rectangle::new(inner.position(), Size::new(btn, btn))),
            add_btn: Some(Rectangle::new(
                Point::new(inner.x + btn + IMAGE_GAP * zoom, inner.y),
                Size::new(btn, btn),
            )),
            remove_btn: (images.len() > 1).then(|| {
                Rectangle::new(Point::new(inner.x + inner.width - btn, inner.y), Size::new(btn, btn))
            }),
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
                    iced::widget::Action::publish(Message::Table(TableMsg::Move { pos: p, size: bounds.size() }))
                })
            }
            Ev::Mouse(MouseEv::ButtonReleased(iced::mouse::Button::Left)) => {
                cursor
                    .position_in(bounds)
                    .map(|p| iced::widget::Action::publish(Message::Table(TableMsg::Release(p))).and_capture())
            }
            Ev::Mouse(MouseEv::WheelScrolled { delta }) => {
                // iced 把全窗口鼠标事件广播给所有 widget：只认光标落在画布内的滚轮（防跨面板串扰）
                if cursor.position_in(bounds).is_none() {
                    return None;
                }
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
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        draw_chrome(self.session, &mut frame, bounds.size());
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
        // 行内选区高亮
        if let Some(region) = region {
            if r >= region.row_start && r <= region.row_end {
                let x0 = g.cache.col_x[region.col_start] - view.scroll_x + g.gutter_w();
                let x1 = g.cache.col_x[region.col_end] + g.cache.col_w[region.col_end] - view.scroll_x + g.gutter_w();
                frame.fill_rectangle(Point::new(x0, y), Size::new(x1 - x0, rh), pal::ACCENT_SOFT);
            }
        }
        // 行号格
        let selected_row = matches!(&view.selection, Some(TableSelection::Row(id)) if data.rows.get(r).is_some_and(|x| &x.id == id));
        frame.fill_rectangle(Point::new(0.0, y), Size::new(g.gutter_w(), rh), if selected_row { pal::BG_TERTIARY } else { pal::BG_SECONDARY });
        if selected_row {
            frame.fill_rectangle(Point::new(0.0, y), Size::new(2.0, rh), pal::ACCENT);
        }
        let num = (r + 1).to_string();
        // 行号靠左、拖拽钮靠右，恒显示不重叠
        fill_text(
            frame,
            &num,
            Point::new(6.0 * g.z, y + (rh - NUM_FONT * g.z * LINE_FACTOR) / 2.0),
            NUM_FONT * g.z,
            mono_font(),
            pal::TEXT_MUTED,
        );
        draw_icon(frame, "grip-vertical", Point::new(g.gutter_w() - 15.0 * g.z, y + rh / 2.0 - 5.0 * g.z), 10.0 * g.z, if selected_row { pal::TEXT_SECONDARY } else { pal::BORDER_STRONG });
        // 行边线
        frame.fill_rectangle(Point::new(0.0, y + rh - 1.0), Size::new(w, 1.0), pal::BORDER_SUBTLE);

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
            let handle = cached.handle.clone();
            let mut img = iced::advanced::image::Image::new(handle);
            img.opacity = 0.92;
            frame.draw_image(rect, img);
        }
    }

    // 「+ 新行」
    let new_row_y = g.cache.content_h - view.scroll_y + g.header_h();
    if new_row_y < g.rows_bottom() {
        let nr = rect(0.0, new_row_y, g.total_w() - view.scroll_x, NEW_ROW_H * g.z);
        let hovered = hover.is_some_and(|p| nr.contains(p));
        if hovered {
            frame.fill_rectangle(nr.position(), nr.size(), pal::HOVER);
        }
        draw_icon(frame, "plus", Point::new(nr.x + 10.0 * g.z, nr.y + (nr.height - 12.0 * g.z) / 2.0), 12.0 * g.z, pal::TEXT_MUTED);
        fill_text(
            frame,
            "新行",
            Point::new(nr.x + 26.0 * g.z, nr.y + (nr.height - NUM_FONT * g.z * LINE_FACTOR) / 2.0),
            NUM_FONT * g.z,
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
            frame.fill_rectangle(Point::new(0.0, y - 1.0), Size::new(w, 2.0), pal::ACCENT);
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
    iced::Font { family: iced::font::Family::Name(crate::ui::FONT_FAMILY), ..iced::Font::default() }
}

fn mono_font() -> iced::Font {
    iced::Font { family: iced::font::Family::Monospace, ..iced::Font::default() }
}

/// 基础文本（左上锚定，Wrapping::None——换行由估算层负责）。
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
            shaping: iced::advanced::text::Shaping::Auto,
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

/// chrome 覆盖层内容：表头 / 状态栏带体 + 滚动条（覆盖在同画布行内容之上，滚动不透底）。
fn draw_chrome(session: &TableSession, frame: &mut canvas::Frame, size: Size) {
    let g = geo(session, size);
    let view = &session.view;
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
}

/// 列头：角钮 + 各列名 + ⋮ + 拖宽把手 + 添加字段列。
fn draw_header(session: &TableSession, g: &Geo, frame: &mut canvas::Frame) {
    let view = &session.view;
    let h = g.header_h();
    let font_size = NUM_FONT * g.z;
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
        let size = NUM_FONT * g.z;
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
                style_font(st),
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
                style_font(st),
                style_color(st),
            );
        }
        _ => {
            let Some(s) = v.and_then(serde_json::Value::as_str) else { return };
            if s.is_empty() {
                return;
            }
            let size = style_size(st, base, g.z);
            let font = iced::Font {
                weight: if st.b { iced::font::Weight::Bold } else { iced::font::Weight::Normal },
                style: if st.i { iced::font::Style::Italic } else { iced::font::Style::Normal },
                ..style_font(st)
            };
            let wrap_w = (cell.width - 2.0 * CELL_PAD_X * g.z).max(8.0);
            let lh = size * LINE_FACTOR;
            let mut y = cell.y + CELL_PAD_Y * g.z;
            for line in wrap_lines_text(s, size, wrap_w) {
                fill_text(frame, &line, Point::new(cell.x + CELL_PAD_X * g.z, y), size, font, style_color(st));
                if st.u {
                    let w = text_width(&line, size).min(wrap_w);
                    frame.fill_rectangle(Point::new(cell.x + CELL_PAD_X * g.z, y + lh - 1.5), Size::new(w, 1.0), style_color(st));
                }
                if st.s {
                    let w = text_width(&line, size).min(wrap_w);
                    frame.fill_rectangle(Point::new(cell.x + CELL_PAD_X * g.z, y + lh / 2.0), Size::new(w, 1.0), style_color(st));
                }
                y += lh;
            }
        }
    }
}

/// 图片单元格（轮播 / 九宫格 + hover 圆钮 + 拖拽影子）。
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
    let layout = image_layout_of(cell, v, g.z);
    let images = table_ops::image_entries(v);
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
        let idx = view.carousel.get(&(row.id.clone(), field.id.clone())).copied().unwrap_or(0).min(images.len().saturating_sub(1));
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
            draw_image_contain(session, frame, &images[page as usize], rect);
        }
        // 队列条
        for (i, (rel, rect)) in layout.thumbs.iter().enumerate() {
            let current = i == idx;
            frame.fill_rectangle(rect.position(), rect.size(), pal::BG_SUNKEN);
            draw_image_contain(session, frame, rel, shrink(*rect, 2.0 * g.z));
            outline(frame, *rect, 2.0 * g.z, if current { pal::ACCENT } else { pal::BORDER });
        }
        if hovered {
            if let Some(btn) = layout.mode_btn {
                circle_btn(frame, btn, "columns-2");
            }
            if let Some(btn) = layout.add_btn {
                circle_btn(frame, btn, "plus");
            }
            if let Some(btn) = layout.remove_btn {
                circle_btn(frame, btn, "x");
            }
        }
    } else {
        for (rel, rect) in &layout.tiles {
            frame.fill_rectangle(rect.position(), rect.size(), pal::BG_SUNKEN);
            draw_image_contain(session, frame, rel, *rect);
        }
        if hovered {
            if let Some(btn) = layout.mode_btn {
                circle_btn(frame, btn, "columns-2");
            }
            if let Some(btn) = layout.add_btn {
                circle_btn(frame, btn, "plus");
            }
        }
        // 拖拽占位（被拖格描边）
        if let Some(drag) = &view.img_drag {
            if drag.row_id == row.id && drag.field_id == field.id {
                if let Some((_, from_rect)) = layout.tiles.get(drag.from) {
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

/// contain 缩放画图（载入时缓存的原始尺寸；未加载 = 不画）。
fn draw_image_contain(session: &TableSession, frame: &mut canvas::Frame, rel: &str, rect: Rectangle) {
    let Some(cached) = session.view.images.get(rel) else { return };
    let handle = cached.handle.clone();
    let (w, h) = cached.size;
    let scale = (rect.width / w).min(rect.height / h);
    let dw = (w * scale).min(rect.width);
    let dh = (h * scale).min(rect.height);
    let bounds = Rectangle::new(
        Point::new(rect.x + (rect.width - dw) / 2.0, rect.y + (rect.height - dh) / 2.0),
        Size::new(dw, dh),
    );
    frame.draw_image(bounds, iced::advanced::image::Image::new(handle));
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

    let canvas: Element<Message> = TableView { session, window }.into();
    let chrome: Element<Message> = TableChrome { session }.into();
    let mut stack = iced::widget::Stack::new().push(canvas).push(chrome);
    if let Some(overlay) = editor_overlay(session) {
        stack = stack.push(overlay);
    }
    if let Some(bar) = format_bar(app, session, window) {
        stack = stack.push(bar);
    }
    if let Some(lb) = &session.view.lightbox {
        stack = stack.push(lightbox_overlay(session, lb, window));
    }

    column![
        toolbar(session, window),
        stack.width(Length::Fill).height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 顶部工具条：行列统计 + 缩放 + ··· 菜单。
fn toolbar<'a>(session: &'a TableSession, window: iced::window::Id) -> Element<'a, Message> {
    let zoom_label = format!("{}%", (session.view.zoom * 100.0).round() as i32);
    let stats = format!("{} 行 · {} 列", session.data.rows.len(), session.data.fields.len());
    let dirty_pill = if session.dirty {
        crate::ui::controls::status_pill(crate::ui::controls::PillStatus::Pending, "未保存")
    } else {
        space().width(Length::Shrink).into()
    };
    let save_state: Element<Message> = if let Some(err) = &session.save_error {
        text(crate::layout_view::ellipsize(err, 220.0, pal::FS_MICRO))
            .size(pal::FS_MICRO)
            .color(pal::DANGER)
            .into()
    } else {
        space().width(Length::Shrink).into()
    };
    row![
        view_select(window),
        dirty_pill,
        space().width(Length::Fill),
        save_state,
        text(zoom_label).size(pal::FS_CAPTION).color(pal::TEXT_MUTED),
        text(stats).size(pal::FS_CAPTION).color(pal::TEXT_SECONDARY),
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
            text("视图").size(pal::FS_CAPTION).color(pal::TEXT_MUTED),
            text("表格").size(pal::FS_CAPTION).color(pal::TEXT_PRIMARY),
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
    let style_color_value = style_color(&st);
    let editor = text_editor(&edit.content)
        .id(iced::widget::Id::new(CELL_EDITOR_ID))
        .on_action(move |a| Message::Table(TableMsg::Edited(a)))
        .key_binding(move |kp| table_key_binding(&kp, is_text))
        .size(style_size(&st, base_font_size(field) * session.view.zoom, session.view.zoom))
        .font(iced::Font {
            weight: if st.b { iced::font::Weight::Bold } else { iced::font::Weight::Normal },
            style: if st.i { iced::font::Style::Italic } else { iced::font::Style::Normal },
            ..style_font(&st)
        })
        .padding(iced::Padding::ZERO.horizontal((CELL_PAD_X - 2.0) * session.view.zoom).vertical(2.0 * session.view.zoom))
        .width(cell.width.max(40.0))
        .height(cell.height.max(24.0))
        .style(move |_t, _s| text_editor::Style {
            background: iced::Background::Color(Color::TRANSPARENT),
            // 编辑框不另画边框：选中框（2px 强调色贴格缘）即编辑态边框（与单元格编辑同款式）
            border: iced::Border { color: Color::TRANSPARENT, width: 0.0, radius: 0.0.into() },
            placeholder: pal::TEXT_MUTED,
            value: style_color_value,
            selection: pal::ACCENT_SOFT,
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

/// 格式气泡（单击已选中格弹出；编辑态 / 换选即隐藏）。
fn format_bar<'a>(app: &'a crate::App, session: &'a TableSession, window: iced::window::Id) -> Option<Element<'a, Message>> {
    let view = &session.view;
    if view.editing.is_some() || !view.bar_shown || view.selection.is_none() {
        return None;
    }
    let region = table_ops::region_of(view.selection.as_ref(), &session.data.fields, &session.data.rows)?;
    let g = geo(session, Size::new(f32::MAX, f32::MAX));
    // 气泡画在锚点上方 34px 处（画布局部坐标），锚点距画布顶不足时隐藏；x 钳制防左溢
    let x0 = (g.cache.col_x[region.col_start] - view.scroll_x + g.gutter_w()).max(8.0);
    let y0 = g.cache.row_y[region.row_start] - view.scroll_y + g.header_h();
    if y0 < BAR_GAP {
        return None;
    }
    let summary = table_ops::style_summary(&session.data.fields, &session.data.rows, &region);
    let bar = row![
        font_pick(app, window, &summary),
        size_pick(window, &summary),
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
    .spacing(4)
    .padding(iced::Padding::ZERO.horizontal(6.0).vertical(4.0));
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
            .padding(iced::Padding::ZERO.left(x0).top(y0 - BAR_GAP))
            .into(),
    )
}

fn divider_el() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(18.0))
        .style(|_t| iced::widget::container::Style {
            background: Some(pal::BORDER.into()),
            ..Default::default()
        })
        .into()
}

fn font_pick<'a>(app: &'a crate::App, window: iced::window::Id, summary: &table_ops::StyleSummary) -> Element<'a, Message> {
    let label = match &summary.font {
        OptState::Mixed => "字体".to_string(),
        OptState::Value(None) => "默认".to_string(),
        OptState::Value(Some(f)) => table_ops_font_label(f),
    };
    let btn = iced::widget::button(text(label).size(pal::FS_CAPTION).color(pal::TEXT_PRIMARY))
        .padding(iced::Padding::ZERO.horizontal(6.0).vertical(3.0))
        .style(bar_btn_style())
        .on_press_maybe(app.table.as_ref().map(|_| Message::Table(TableMsg::OpenFontMenu(window))));
    btn.into()
}

fn size_pick(window: iced::window::Id, summary: &table_ops::StyleSummary) -> Element<'static, Message> {
    let label = match &summary.size {
        OptState::Mixed => "字号".to_string(),
        OptState::Value(None) => "字号".to_string(),
        OptState::Value(Some(s)) => format!("{}", *s as i32),
    };
    iced::widget::button(text(label).size(pal::FS_CAPTION).color(pal::TEXT_PRIMARY))
        .padding(iced::Padding::ZERO.horizontal(6.0).vertical(3.0))
        .style(bar_btn_style())
        .on_press(Message::Table(TableMsg::OpenSizeMenu(window)))
        .into()
}

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
        _ => "默认",
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
    iced::widget::button(text(label).size(pal::FS_CAPTION).color(if on { pal::ACCENT } else { pal::TEXT_PRIMARY }))
        .padding(iced::Padding::ZERO.horizontal(6.0).vertical(3.0))
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
        .width(Length::Fixed(10.0))
        .height(Length::Fixed(10.0))
        .style(move |_t| iced::widget::container::Style {
            background: Some(current.unwrap_or(pal::TEXT_PRIMARY).into()),
            border: iced::border::rounded(pal::RADIUS_XS).color(pal::BORDER_STRONG).width(1.0),
            ..Default::default()
        });
    let label = if bg { "底" } else { "字" };
    let btn = iced::widget::button(row![text(label).size(pal::FS_CAPTION).color(pal::TEXT_PRIMARY), swatch].spacing(3))
        .padding(iced::Padding::ZERO.horizontal(6.0).vertical(3.0))
        .style(bar_btn_style())
        .on_press_maybe(
            app.table
                .as_ref()
                .map(|_| Message::Table(TableMsg::OpenColorMenu { window, bg })),
        );
    btn.into()
}

fn clear_btn() -> Element<'static, Message> {
    iced::widget::button(crate::ui::icon::icon("eraser", 13.0, pal::TEXT_PRIMARY))
        .padding(iced::Padding::ZERO.horizontal(5.0).vertical(3.0))
        .style(bar_btn_style())
        .on_press(Message::Table(TableMsg::ClearFormat))
        .into()
}

/// 灯箱：点击图片外关闭；右键图片弹下载菜单；多图显示左右翻页与计数。
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
    let handle = images.get(index).and_then(|rel| session.view.images.get(*rel)).map(|c| c.handle.clone());
    let img_size = images
        .get(index)
        .and_then(|rel| session.view.images.get(*rel))
        .map(|c| c.size);
    let multi = !empty && images.len() > 1;
    // 图片 widget 按实际比例与可视区算尺寸（Contain 顶格会让点击关闭区域归零）：
    // 四周留 48px 遮罩边距，点击遮罩关闭；图片本体吞按压、右键弹下载菜单
    let big: Element<'_, Message> = iced::widget::responsive(move |size| {
        if empty {
            return container(text("图片已清空").size(pal::FS_CAPTION).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .height(Length::Fill)
                .center(iced::Length::Fill)
                .into();
        }
        let inner = Size::new(
            (size.width - 2.0 * LIGHTBOX_MARGIN).max(1.0),
            (size.height - 2.0 * LIGHTBOX_MARGIN).max(1.0),
        );
        match (&handle, img_size) {
            (Some(h), Some((w, h_px))) if w > 0.0 && h_px > 0.0 => {
                let scale = (inner.width / w).min(inner.height / h_px);
                let (dw, dh) = (w * scale, h_px * scale);
                container(
                    mouse_area(
                        iced::widget::image(h.clone())
                            .width(Length::Fixed(dw))
                            .height(Length::Fixed(dh)),
                    )
                    .on_press(Message::Table(TableMsg::Noop))
                    .on_right_press(Message::Table(TableMsg::OpenLightboxMenu(window))),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .center(iced::Length::Fill)
                .into()
            }
            _ => container(text("图片加载中").size(pal::FS_CAPTION).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .height(Length::Fill)
                .center(iced::Length::Fill)
                .into(),
        }
    })
    .into();
    // 遮罩：图片铺满可视区（Contain 居中），按压落在图片外 = 关闭
    let mask = mouse_area(
        container(big)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| iced::widget::container::Style {
                background: Some(iced::Background::Color(pal::SCRIM_STRONG)),
                ..Default::default()
            }),
    )
    .on_press(Message::Table(TableMsg::LightboxClose));

    let mut stack = iced::widget::Stack::new().push(mask);
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
            container(text(format!("{}/{}", index + 1, images.len())).size(pal::FS_CAPTION).color(pal::TEXT_PRIMARY))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(alignment::Vertical::Bottom)
                .align_x(alignment::Horizontal::Center)
                .padding(8.0),
        );
    }
    stack.width(Length::Fill).height(Length::Fill).into()
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
