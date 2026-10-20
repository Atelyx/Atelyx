//! 画布视图：视口（平移 / 缩放 / 适应视图）+ 卡与连线自绘 + 对话卡消息流浮层。
//! 世界层为 iced canvas 自绘（网格与内容两层缓存），富内容浮层以原生控件叠 Stack；
//! 命中几何是世界空间纯函数。

use std::collections::{HashMap, HashSet};

use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::text::Shaping;
use iced::advanced::widget::{self, Widget};
use iced::advanced::{Clipboard, Renderer as _, Shell};
use iced::event::Event;
use iced::widget::canvas::{
    self, Canvas, Frame, LineDash, Path, Stroke, Text as CanvasText,
};
use iced::widget::{column, container, responsive, row, space, stack};
use iced::{alignment, Color, Length, Padding, Point, Rectangle, Size, Vector};
use serde_json::Value;

use crate::ui::icon::{icon, polylines};
use crate::ui::{controls, palette as pal};
use crate::ui::scroll;
use crate::ui::text;
use crate::{layout_view, App, Message};

// ===== 常量 =====

/// 适应视图的世界包围盒边距（屏幕像素）。
const FIT_PAD: f32 = 48.0;
/// 缩放上下限与滚轮档系数。
pub(crate) const MIN_ZOOM: f32 = 0.1;
pub(crate) const MAX_ZOOM: f32 = 3.0;
pub(crate) const ZOOM_STEP: f32 = 1.12;
/// 滚轮行模式每行像素。
pub(crate) const WHEEL_LINE_PX: f32 = 40.0;
/// 滚轮缩放灵敏度（滚动分量 → 指数系数：一档 40px 约放大 10%）。
pub(crate) const WHEEL_ZOOM_K: f32 = 0.0025;
/// resize 把手边长（屏幕像素）。
const HANDLE_SIZE: f32 = 9.0;
/// 卡圆角与头高（世界坐标）。
const CARD_RADIUS: f32 = 10.0;
/// 分组容器圆角（与卡区分：分组更圆）。
const GROUP_RADIUS: f32 = 14.0;
pub(crate) const CARD_HEADER_H: f32 = 30.0;
/// 卡内边距（世界坐标）。
const CARD_PAD: f32 = 10.0;
/// 卡可缩放到的最小尺寸（世界坐标）。
pub(crate) const CARD_MIN_W: f64 = 120.0;
pub(crate) const CARD_MIN_H: f64 = 80.0;
/// 世界网格步长。
const GRID_STEP: f64 = 100.0;
/// 卡正文预览行数上限。
const PREVIEW_LINES: usize = 7;
/// 工具条高度（屏幕像素）。
const TOOLBAR_H: f32 = 36.0;

/// 画布保存动机（与表格 SaveReason 同构）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveReason {
    Manual,
    AutoSave,
    Flush,
}

// ===== 视口 =====

/// 生效视口：世界 ↔ 屏幕的线性变换（screen = world × zoom + pan）。
#[derive(Clone, Copy)]
pub struct View {
    pub zoom: f32,
    pub pan: (f32, f32),
}

impl View {
    pub fn of(view: &CanvasViewState) -> Self {
        Self { zoom: view.zoom, pan: view.pan }
    }

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
fn default_size(node_type: &str) -> (f64, f64) {
    match node_type {
        "conversation" => (520.0, 420.0),
        "table" => (300.0, 110.0),
        "link" => (300.0, 100.0),
        "group" => (400.0, 300.0),
        _ => (360.0, 240.0),
    }
}

/// 卡显示尺寸（width/height 缺省按类型回落）。
pub(crate) fn node_size(n: &atelyx_core::vault::CanvasFileNode) -> (f64, f64) {
    let (dw, dh) = default_size(&n.node_type);
    (n.width.unwrap_or(dw), n.height.unwrap_or(dh))
}

/// 适应视图变换（纯函数）：全部内容卡包围盒装入视口，无卡 = 复位原点。
pub fn fit_transform(
    nodes: &[atelyx_core::vault::CanvasFileNode],
    card_size: impl Fn(&atelyx_core::vault::CanvasFileNode) -> (f64, f64),
    vw: f32,
    vh: f32,
) -> (f32, (f32, f32)) {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for n in nodes {
        if n.node_type == "group" {
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

// ===== 会话状态 =====

/// 画布会话：磁盘真源的内存投影（对称表格会话）。
pub struct CanvasSession {
    /// 相对仓库根路径。
    pub file: String,
    pub data: atelyx_core::vault::CanvasFile,
    /// 落盘基线（上次保存成功时的快照；补丁 diff 源）。
    pub saved: atelyx_core::vault::CanvasFile,
    pub dirty: bool,
    pub save_error: Option<String>,
    /// 数据变更代：任何 data 变更自增（自动保存触发判据；视图态变化不动它）。
    pub version: u64,
    pub view: CanvasViewState,
}

impl CanvasSession {
    pub fn new(file: String, data: atelyx_core::vault::CanvasFile) -> Self {
        let saved = data.clone();
        Self {
            file,
            data,
            saved,
            dirty: false,
            save_error: None,
            version: 1,
            view: CanvasViewState::default(),
        }
    }

    /// 卡显示尺寸。
    pub fn card_size(&self, n: &atelyx_core::vault::CanvasFileNode) -> (f64, f64) {
        node_size(n)
    }

    /// 数据已变更：置脏 + 版本自增 + 内容缓存失效。
    pub fn touch(&mut self) {
        self.dirty = true;
        self.save_error = None;
        self.version += 1;
        self.view.content_cache.clear();
    }

    /// 仅重绘（视图态变化，不动数据也不动版本代）。
    pub fn redraw(&mut self) {
        self.view.content_cache.clear();
    }

    /// 请求适应视图：视图层随即按适配变换渲染，指针事件收口进会话状态。
    pub fn request_fit(&mut self) {
        self.view.fit_pending = true;
        self.view.grid_cache.clear();
        self.redraw();
    }

    /// 以已知视口尺寸提交适应视图。
    pub fn apply_fit(&mut self, vw: f32, vh: f32) {
        let (zoom, pan) = fit_transform(&self.data.nodes, |n| self.card_size(n), vw, vh);
        self.view.zoom = zoom;
        self.view.pan = pan;
        self.view.fit_pending = false;
        self.view.grid_cache.clear();
        self.redraw();
    }
}

/// 画布视图状态（视口 / 交互 / 对话卡只读流的渲染缓存）。
pub struct CanvasViewState {
    /// 世界 → 屏幕缩放。
    pub zoom: f32,
    /// 世界原点的屏幕偏移：screen = world * zoom + pan。
    pub pan: (f32, f32),
    /// 适应视图待收口（视图层先按适配变换渲染，首个带视口尺寸的指针事件提交）。
    pub fit_pending: bool,
    /// 选中卡 id（单选）。
    pub selection: Vec<String>,
    /// 进行中的拖拽会话（平移 / 卡拖动 / resize）。
    pub drag: Option<DragState>,
    /// 对话卡折叠块展开集（键 = 消息 id + 步骤序）。
    pub expanded: HashSet<String>,
    /// 对话卡消息流 markdown 解析缓存（键 = 源哈希；载入时一次性预解析）。
    pub docs: HashMap<u64, atelyx_core::markdown::MarkdownDocument>,
    /// 文本卡正文解析结果（键 = 卡 id；正文由原生浮层渲染，载入时一次性预解析）。
    pub text_docs: HashMap<String, atelyx_core::markdown::MarkdownDocument>,
    /// 卡引用内容缓存（键 = 仓库相对路径；载入后异步回填）。
    pub refs: HashMap<String, RefContent>,
    /// 网格层缓存（随平移缩放失效）。
    pub grid_cache: canvas::Cache,
    /// 内容层缓存（随数据或视图态变化失效）。
    pub content_cache: canvas::Cache,
}

impl Default for CanvasViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: (40.0, 40.0),
            fit_pending: false,
            selection: Vec::new(),
            drag: None,
            expanded: HashSet::new(),
            docs: HashMap::new(),
            text_docs: HashMap::new(),
            refs: HashMap::new(),
            grid_cache: canvas::Cache::new(),
            content_cache: canvas::Cache::new(),
        }
    }
}

/// 拖拽会话：起始屏幕点 + 各受影响卡的起始几何（width/height 已按显示尺寸展开）。
pub struct DragState {
    pub kind: DragKind,
    pub start: (f32, f32),
    /// 起始几何（id → (x, y, width, height)）。
    pub orig: HashMap<String, (f64, f64, f64, f64)>,
}

#[derive(Clone)]
pub enum DragKind {
    /// 空白处拖拽 = 平移视口。
    Pan,
    /// 卡拖动（松手落盘）。
    Card,
    /// 右下角 resize（松手落盘）。
    Resize(String),
}

/// 卡引用内容（载入后异步回填；媒体字节转句柄在回执侧完成）。
pub enum RefContent {
    /// 笔记 / 文本附件正文。
    Text(String),
    /// 图片句柄 + 原始像素尺寸（等比绘制用）。
    Image {
        handle: iced::widget::image::Handle,
        width: u32,
        height: u32,
    },
    /// 表格薄卡摘要（字段数 · 行数）。
    Table { summary: String },
    /// 二进制附件（非文本、非图片，只显示占位说明）。
    Binary,
    /// 引用缺失（外部删除 / 读取失败）。
    Missing,
}

// ===== 命中几何（世界空间纯函数） =====

/// 命中判定：顶层卡优先（数组逆序），分组最低优先（组内卡可点穿）。返回节点下标。
pub fn hit_card(canvas: &atelyx_core::vault::CanvasFile, world: (f64, f64)) -> Option<usize> {
    let contains = |n: &atelyx_core::vault::CanvasFileNode| {
        let (w, h) = node_size(n);
        world.0 >= n.x && world.0 <= n.x + w && world.1 >= n.y && world.1 <= n.y + h
    };
    // 分组置于其后判定（组矩形内的卡优先）
    canvas
        .nodes
        .iter()
        .rposition(|n| n.node_type != "group" && contains(n))
        .or_else(|| canvas.nodes.iter().rposition(|n| n.node_type == "group" && contains(n)))
}

/// 右下角 resize 把手命中（仅选中卡有把手；世界坐标容差按缩放反推屏幕像素）。
pub fn hit_resize_handle(
    canvas: &atelyx_core::vault::CanvasFile,
    selection: &[String],
    zoom: f32,
    world: (f64, f64),
) -> Option<String> {
    let tol = HANDLE_SIZE as f64 / zoom as f64;
    for n in canvas.nodes.iter().rev() {
        if n.node_type == "group" || !selection.iter().any(|s| s == &n.id) {
            continue;
        }
        let (w, h) = node_size(n);
        let (hx, hy) = (n.x + w, n.y + h);
        if world.0 >= hx - tol && world.0 <= hx + tol && world.1 >= hy - tol && world.1 <= hy + tol {
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

/// 从旧句柄名（如 "right-source" / "left-target"）取侧名；缺失 = None（回落最近侧）。
fn handle_side(handle: &Option<String>) -> Option<&str> {
    let h = handle.as_deref()?;
    let side = h.split('-').next()?;
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

/// 连线两端锚点（世界坐标）：句柄缺失时按对端方向取最近侧。
pub fn edge_endpoints(
    canvas: &atelyx_core::vault::CanvasFile,
    e: &atelyx_core::vault::CanvasFileEdge,
) -> Option<((f64, f64), (f64, f64))> {
    let s = canvas.nodes.iter().find(|n| n.id == e.source)?;
    let t = canvas.nodes.iter().find(|n| n.id == e.target)?;
    let (sw, sh) = node_size(s);
    let (tw, th) = node_size(t);
    let s_side = handle_side(&e.source_handle)
        .map(|s| s.to_string())
        .unwrap_or_else(|| nearest_side(s.x, s.y, sw, sh, (t.x + tw / 2.0, t.y + th / 2.0)).to_string());
    let t_side = handle_side(&e.target_handle)
        .map(|s| s.to_string())
        .unwrap_or_else(|| nearest_side(t.x, t.y, tw, th, (s.x + sw / 2.0, s.y + sh / 2.0)).to_string());
    Some((
        anchor_of(s.x, s.y, sw, sh, &s_side),
        anchor_of(t.x, t.y, tw, th, &t_side),
    ))
}

/// 数据流连线是否已被消费：目标对话卡任一消息的引用（旧格式 refs[].nodeId）命中来源卡。
/// `directed != false` 即数据流连线（磁盘缺省 = 数据流）。
pub fn edge_consumed(canvas: &atelyx_core::vault::CanvasFile, e: &atelyx_core::vault::CanvasFileEdge) -> bool {
    if e.directed == Some(false) {
        return false;
    }
    let Some(target) = canvas.nodes.iter().find(|n| n.id == e.target) else {
        return false;
    };
    if target.node_type != "conversation" {
        return false;
    }
    let Some(messages) = target.data.get("messages").and_then(Value::as_array) else {
        return false;
    };
    messages.iter().any(|m| {
        m.get("refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|r| r.get("nodeId").and_then(Value::as_str) == Some(e.source.as_str()))
    })
}

/// 连线是否为数据流连线（directed 缺省 = 数据流）。
fn is_dataflow(e: &atelyx_core::vault::CanvasFileEdge) -> bool {
    e.directed != Some(false)
}

// ===== 消息 =====

#[derive(Debug, Clone)]
pub enum CanvasMsg {
    /// 载入回执（gen 守卫；含预解析的消息流文档与引用内容）。
    Loaded(u64, String, Result<LoadOut, String>),
    /// 保存回执（冻结快照供「落盘期间无新编辑」判定）。
    Saved(u64, String, SaveReason, Result<(), String>, atelyx_core::vault::CanvasFile),
    Press { pos: Point, size: Size },
    /// 中键按下 = 平移起手（不改变选中；卡上也可起手平移）。
    PanPress { pos: Point, size: Size },
    Move { pos: Point, size: Size },
    Release,
    Wheel { delta_y: f32, delta_x: f32, pos: Point, size: Size },
    /// 工具条缩放钮（视口中心锚定；尺寸由视图组装时携带）。
    Zoom { factor: f32, size: Size },
    /// 适应视图（覆盖变换渲染，指针事件收口；无需携带尺寸）。
    FitView,
    /// 对话卡折叠块开合（键 = 消息 id + 步骤序）。
    StreamToggle(String),
}

/// 载入产物：画布数据 + 预解析的 markdown 文档（对话消息流 / 文本卡正文）+ 引用内容。
#[derive(Debug, Clone)]
pub struct LoadOut {
    pub canvas: atelyx_core::vault::CanvasFile,
    pub docs: HashMap<u64, atelyx_core::markdown::MarkdownDocument>,
    /// 文本卡正文（键 = 卡 id；引用缺失 / 二进制 / 空正文的卡不入表）。
    pub text_docs: HashMap<String, atelyx_core::markdown::MarkdownDocument>,
    /// 引用内容（键 = 仓库相对路径；图片为原始字节 + 像素尺寸，回执侧转句柄）。
    pub refs: HashMap<String, RefRaw>,
}

/// 引用内容原始形态（载入任务内读取；图片在回执侧转渲染句柄）。
#[derive(Debug, Clone)]
pub enum RefRaw {
    Text(String),
    /// 图片字节 + 像素尺寸（解码失败 = 0，回执侧按占位绘制）。
    Image { bytes: Vec<u8>, width: u32, height: u32 },
    Table { summary: String },
    /// 存在但既非文本也非图片（二进制附件）。
    Binary,
    Missing,
}

// ===== 部件 =====

/// 画布世界层自绘部件（State = ()；会话态在 App.canvas，视口由视图组装传入——
/// 适应视图待收口期间为适配变换，与后续提交值同源，显示不跳变）。
pub struct WorldLayer<'a> {
    pub session: &'a CanvasSession,
    pub view: View,
}

impl canvas::Program<Message> for WorldLayer<'_> {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<iced::widget::Action<Message>> {
        use canvas::Event as Ev;
        match event {
            Ev::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                cursor.position_in(bounds).map(|p| {
                    iced::widget::Action::publish(Message::Canvas(CanvasMsg::Press {
                        pos: p,
                        size: bounds.size(),
                    }))
                    .and_capture()
                })
            }
            // 中键 = 平移起手（无论光标下是什么卡）
            Ev::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Middle)) => {
                cursor.position_in(bounds).map(|p| {
                    iced::widget::Action::publish(Message::Canvas(CanvasMsg::PanPress {
                        pos: p,
                        size: bounds.size(),
                    }))
                    .and_capture()
                })
            }
            Ev::Mouse(iced::mouse::Event::CursorMoved { .. }) => {
                // 拖拽中事件随捕获跟手（光标可越出画布区）；非拖拽只在光标于画布内时上报
                let p = if self.session.view.drag.is_some() {
                    // 拖拽捕获期间光标可越出画布：绝对坐标换算回 bounds 相对
                    cursor.position().map(|a| Point::new(a.x - bounds.x, a.y - bounds.y))
                } else {
                    cursor.position_in(bounds)
                };
                p.map(|p| {
                    iced::widget::Action::publish(Message::Canvas(CanvasMsg::Move {
                        pos: p,
                        size: bounds.size(),
                    }))
                })
            }
            Ev::Mouse(iced::mouse::Event::ButtonReleased(
                iced::mouse::Button::Left | iced::mouse::Button::Middle,
            )) => {
                // 拖拽会话必须收口：按下时已捕获，抬起无论光标在不在画布内都发布
                Some(iced::widget::Action::publish(Message::Canvas(CanvasMsg::Release)).and_capture())
            }
            Ev::Mouse(iced::mouse::Event::WheelScrolled { delta }) => {
                // iced 把全窗口鼠标事件广播给所有 widget：只认光标落在画布内的滚轮（防跨面板串扰）
                let p = cursor.position_in(bounds)?;
                let (dx, dy) = match delta {
                    iced::mouse::ScrollDelta::Lines { x, y } => (x * WHEEL_LINE_PX, y * WHEEL_LINE_PX),
                    iced::mouse::ScrollDelta::Pixels { x, y } => (*x, *y),
                };
                Some(iced::widget::Action::publish(Message::Canvas(CanvasMsg::Wheel {
                    delta_y: dy,
                    delta_x: dx,
                    pos: p,
                    size: bounds.size(),
                })))
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
        let grid = self.session.view.grid_cache.draw(renderer, bounds.size(), |frame| {
            draw_grid(&self.view, frame, bounds.size());
        });
        let content = self.session.view.content_cache.draw(renderer, bounds.size(), |frame| {
            draw_world(self.session, &self.view, frame);
        });
        vec![grid, content]
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
        if let Some(drag) = &self.session.view.drag {
            return match drag.kind {
                DragKind::Pan => iced::mouse::Interaction::Grabbing,
                DragKind::Card => iced::mouse::Interaction::Grabbing,
                DragKind::Resize(_) => iced::mouse::Interaction::ResizingDiagonallyUp,
            };
        }
        let world = self.view.world(p.x, p.y);
        if hit_resize_handle(&self.session.data, &self.session.view.selection, self.view.zoom, world).is_some() {
            return iced::mouse::Interaction::ResizingDiagonallyUp;
        }
        if hit_card(&self.session.data, world).is_some() {
            return iced::mouse::Interaction::Grab;
        }
        iced::mouse::Interaction::default()
    }
}

impl<'a> From<WorldLayer<'a>> for Element<'a> {
    fn from(w: WorldLayer<'a>) -> Element<'a> {
        Canvas::new(w).width(Length::Fill).height(Length::Fill).into()
    }
}

type Element<'a> = iced::Element<'a, Message>;

// ===== 绘制 =====

/// 网格层：世界主网格线（随视口取整对齐）。
fn draw_grid(view: &View, frame: &mut Frame, size: Size) {
    let step = GRID_STEP * view.zoom as f64;
    if step < 12.0 {
        // 过密不画（深缩放时退化为纯背景）
        return;
    }
    let x0 = view.pan.0.rem_euclid(step as f32);
    let y0 = view.pan.1.rem_euclid(step as f32);
    let mut x = x0;
    while x <= size.width {
        let line = Path::line(Point::new(x, 0.0), Point::new(x, size.height));
        frame.stroke(
            &line,
            Stroke {
                style: canvas::Style::Solid(pal::BORDER_SUBTLE),
                width: 1.0,
                ..Stroke::default()
            },
        );
        x += step as f32;
    }
    let mut y = y0;
    while y <= size.height {
        let line = Path::line(Point::new(0.0, y), Point::new(size.width, y));
        frame.stroke(
            &line,
            Stroke {
                style: canvas::Style::Solid(pal::BORDER_SUBTLE),
                width: 1.0,
                ..Stroke::default()
            },
        );
        y += step as f32;
    }
}

/// 内容层：分组 → 连线 → 卡（数据序，后画者在上）。
fn draw_world(session: &CanvasSession, view: &View, frame: &mut Frame) {
    let sel: HashSet<&str> = session.view.selection.iter().map(|s| s.as_str()).collect();

    // 分组背景（组内卡之上无内容，先画沉底）
    for n in &session.data.nodes {
        if n.node_type != "group" {
            continue;
        }
        let (w, h) = session.card_size(n);
        draw_group(frame, view, n, w, h, sel.contains(n.id.as_str()));
    }

    // 连线
    for e in &session.data.edges {
        draw_edge(session, view, frame, e);
    }

    // 卡（非分组，数据序）
    for n in &session.data.nodes {
        if n.node_type == "group" {
            continue;
        }
        let (w, h) = session.card_size(n);
        draw_card(session, view, frame, n, w, h, sel.contains(n.id.as_str()));
    }
}

/// 圆角矩形（`x`/`y` 为左上角；iced 的 `rounded_rectangle` 取左上角坐标）。
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    Path::new(|p| {
        p.rounded_rectangle(Point::new(x, y), Size::new(w, h), r.into());
    })
}

/// 顶部两角圆角、底边平直的矩形（半透明底上不能靠补画直角覆盖——叠涂会显深色带）。
fn top_rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w / 2.0).min(h);
    Path::new(|p| {
        p.move_to(Point::new(x, y + h));
        p.line_to(Point::new(x, y + r));
        p.arc_to(Point::new(x, y), Point::new(x + r, y), r);
        p.line_to(Point::new(x + w - r, y));
        p.arc_to(Point::new(x + w, y), Point::new(x + w, y + r), r);
        p.line_to(Point::new(x + w, y + h));
        p.close();
    })
}

/// 分组虚线描边的间隔（屏幕像素；不随缩放，低缩放下不致虚线不可见）。
const GROUP_DASH: [f32; 2] = [6.0, 4.0];

/// 分组：半透明色块 + 虚线描边 + 顶部标题行（组色 25% 底、主文字色标题）。
fn draw_group(frame: &mut Frame, view: &View, n: &atelyx_core::vault::CanvasFileNode, w: f64, h: f64, selected: bool) {
    let (sx, sy) = view.screen(n.x, n.y);
    let (sw, sh) = (w as f32 * view.zoom, h as f32 * view.zoom);
    if sw < 2.0 || sh < 2.0 {
        return;
    }
    let z = view.zoom;
    let color = group_color(n.data.get("color").and_then(Value::as_str));
    let rect = rounded_rect(sx, sy, sw, sh, GROUP_RADIUS * z);
    frame.fill(&rect, iced::Color { a: 0.12, ..color });
    let border_color = if selected { pal::ACCENT } else { color };
    frame.stroke(
        &rect,
        Stroke {
            style: canvas::Style::Solid(border_color),
            width: if selected { 2.0 } else { 1.5 },
            line_dash: LineDash { segments: &GROUP_DASH, offset: 0 },
            ..Stroke::default()
        },
    );
    // 标题行：组色 25% 底（顶部两角随容器圆角、底边平直，与下方组底不叠涂出深色带）
    let head_h = (CARD_HEADER_H * z).min(sh);
    let head = top_rounded_rect(sx, sy, sw, head_h, GROUP_RADIUS * z);
    frame.fill(&head, iced::Color { a: 0.25, ..color });
    let label = n.data.get("label").and_then(Value::as_str).unwrap_or("");
    if !label.is_empty() {
        let size = pal::fs_body() * z;
        frame.fill_text(canvas::Text {
            content: layout_view::ellipsize(label, sw - CARD_PAD * 2.0 * z, size),
            position: Point::new(sx + CARD_PAD * z, sy + (head_h - size * 1.3) / 2.0),
            max_width: f32::INFINITY,
            color: pal::TEXT_PRIMARY,
            size: size.into(),
            line_height: iced::advanced::text::LineHeight::Relative(1.3),
            font: crate::ui::bold(),
            align_x: iced::advanced::text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::Advanced,
        });
    }
}

/// 数字色板索引（"1"-"6"）→ 分组色（磁盘 data.color 缺省 / 越界 = 中性灰）。
fn group_color(v: Option<&str>) -> Color {
    v.and_then(|s| s.parse::<usize>().ok())
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| pal::CANVAS_GROUP_COLORS.get(i).copied())
        .unwrap_or(pal::TEXT_MUTED)
}

/// 连线：数据流（金，虚 = 未消费）与关联（灰，箭头按 linkMode）。
fn draw_edge(session: &CanvasSession, view: &View, frame: &mut Frame, e: &atelyx_core::vault::CanvasFileEdge) {
    let Some(((x0, y0), (x1, y1))) = edge_endpoints(&session.data, e) else {
        return;
    };
    let (p0x, p0y) = view.screen(x0, y0);
    let (p1x, p1y) = view.screen(x1, y1);
    let (p0, p1) = (Point::new(p0x, p0y), Point::new(p1x, p1y));
    // 三次贝塞尔：控制点沿两端各自外向延伸
    let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
    let dist = (dx * dx + dy * dy).sqrt().max(1.0);
    let ext = (dist / 2.0).min(90.0);
    let (s_side, t_side) = (edge_side(session, e, true), edge_side(session, e, false));
    let (s_out, t_out) = (side_normal(s_side), side_normal(t_side));
    let c0 = Point::new(p0.x + s_out.0 * ext, p0.y + s_out.1 * ext);
    let c1 = Point::new(p1.x + t_out.0 * ext, p1.y + t_out.1 * ext);
    let path = Path::new(|p| {
        p.move_to(p0);
        p.bezier_curve_to(c0, c1, p1);
    });
    let (color, width, dash) = if is_dataflow(e) {
        let consumed = edge_consumed(&session.data, e);
        (pal::ACCENT, if consumed { 1.8 } else { 1.4 }, !consumed)
    } else {
        (pal::BORDER_STRONG, 1.2, false)
    };
    frame.stroke(
        &path,
        Stroke {
            style: canvas::Style::Solid(color),
            width,
            line_dash: if dash {
                LineDash { segments: &[3.0, 4.0], offset: 0 }
            } else {
                LineDash { segments: &[], offset: 0 }
            },
            ..Stroke::default()
        },
    );
    // 箭头：数据流恒单箭头（指向消费方）；关联边按 linkMode
    let arrows = if is_dataflow(e) {
        1
    } else {
        match e.link_mode.as_deref() {
            Some("single") => 1,
            Some("double") => 2,
            _ => 0,
        }
    };
    if arrows >= 1 {
        draw_arrow(frame, p1, (-t_out.0, -t_out.1), color);
    }
    if arrows >= 2 {
        draw_arrow(frame, p0, (-s_out.0, -s_out.1), color);
    }
}

/// 连线端所属侧（句柄缺失回落最近侧的再推导）。
fn edge_side(session: &CanvasSession, e: &atelyx_core::vault::CanvasFileEdge, source: bool) -> &'static str {
    let (id, handle) = if source {
        (&e.source, &e.source_handle)
    } else {
        (&e.target, &e.target_handle)
    };
    let Some(n) = session.data.nodes.iter().find(|n| &n.id == id) else {
        return "right";
    };
    if let Some(side) = handle_side(handle) {
        return match side {
            "left" => "left",
            "top" => "top",
            "bottom" => "bottom",
            _ => "right",
        };
    }
    let (w, h) = session.card_size(n);
    let other_id = if source { &e.target } else { &e.source };
    let toward = session
        .data
        .nodes
        .iter()
        .find(|n| &n.id == other_id)
        .map(|o| {
            let (ow, oh) = session.card_size(o);
            (o.x + ow / 2.0, o.y + oh / 2.0)
        })
        .unwrap_or((n.x, n.y));
    nearest_side(n.x, n.y, w, h, toward)
}

/// 侧外向法线（世界方向，屏幕同向）。
fn side_normal(side: &str) -> (f32, f32) {
    match side {
        "left" => (-1.0, 0.0),
        "top" => (0.0, -1.0),
        "bottom" => (0.0, 1.0),
        _ => (1.0, 0.0),
    }
}

/// 箭头三角（tip 在端点上，指向 dir 方向）。
fn draw_arrow(frame: &mut Frame, tip: Point, dir: (f32, f32), color: Color) {
    let len = 9.0;
    let half = 4.0;
    // 法线方向
    let (nx, ny) = (-dir.1, dir.0);
    let base = Point::new(tip.x - dir.0 * len, tip.y - dir.1 * len);
    let a = Point::new(base.x + nx * half, base.y + ny * half);
    let b = Point::new(base.x - nx * half, base.y - ny * half);
    frame.fill(
        &Path::new(|p| {
            p.move_to(tip);
            p.line_to(a);
            p.line_to(b);
            p.close();
        }),
        color,
    );
}

/// 卡框体：背景 + 头部（图标 + 标题）+ 选中描边与 resize 把手；正文按类型分派。
fn draw_card(
    session: &CanvasSession,
    view: &View,
    frame: &mut Frame,
    n: &atelyx_core::vault::CanvasFileNode,
    w: f64,
    h: f64,
    selected: bool,
) {
    let (sx, sy) = view.screen(n.x, n.y);
    let (sw, sh) = (w as f32 * view.zoom, h as f32 * view.zoom);
    if sw < 2.0 || sh < 2.0 {
        return;
    }
    let (icon_name, title) = card_head(n);
    let z = view.zoom;

    // 框体
    frame.fill(&rounded_rect(sx, sy, sw, sh, CARD_RADIUS * z), pal::BG_SECONDARY);
    let border_color = if selected { pal::ACCENT } else { pal::BORDER };
    frame.stroke(
        &rounded_rect(sx, sy, sw, sh, CARD_RADIUS * z),
        Stroke {
            style: canvas::Style::Solid(border_color),
            width: if selected { 2.0 } else { 1.0 },
            ..Stroke::default()
        },
    );

    // 头部条
    let head_h = CARD_HEADER_H * z;
    frame.fill(&rounded_rect(sx, sy, sw, head_h, CARD_RADIUS * z), pal::BG_TERTIARY);
    // 圆角矩形下半覆盖为直角（头底边平直）
    frame.fill(
        &Path::rectangle(Point::new(sx, sy + head_h / 2.0), Size::new(sw, head_h / 2.0)),
        pal::BG_TERTIARY,
    );
    draw_icon_glyph(frame, icon_name, sx + 9.0 * z, sy + (head_h - 12.0 * z) / 2.0, 12.0 * z, pal::TEXT_MUTED);
    let title_size = pal::fs_caption() * z;
    let title_x = sx + 26.0 * z;
    frame.fill_text(CanvasText {
        content: layout_view::ellipsize(&title, (sx + sw - title_x - 8.0 * z).max(0.0), title_size),
        position: Point::new(title_x, sy + (head_h - title_size * 1.3) / 2.0),
        max_width: f32::INFINITY,
        color: pal::TEXT_PRIMARY,
        size: title_size.into(),
        line_height: iced::advanced::text::LineHeight::Relative(1.3),
        font: crate::ui::bold(),
        align_x: iced::advanced::text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
    });

    // 正文区（对话卡消息流与文本卡正文由原生浮层渲染，见 card_overlays）
    let body_y = sy + head_h;
    let body = Size::new(sw - CARD_PAD * 2.0 * z, sh - head_h - CARD_PAD * 2.0 * z);
    draw_card_body(
        session,
        view,
        frame,
        n,
        Rectangle::new(Point::new(sx + CARD_PAD * z, body_y + CARD_PAD * z), body.max(Size::ZERO)),
    );

    // 选中 resize 把手（右下角）
    if selected {
        let hs = HANDLE_SIZE;
        frame.fill(
            &Path::rectangle(Point::new(sx + sw - hs, sy + sh - hs), Size::new(hs, hs)),
            pal::ACCENT,
        );
    }
}

/// 卡头部（图标名 + 标题）。
fn card_head(n: &atelyx_core::vault::CanvasFileNode) -> (&'static str, String) {
    match n.node_type.as_str() {
        "conversation" => ("message-square", n.data.get("title").and_then(Value::as_str).unwrap_or("对话").to_string()),
        "text" => match n.data.get("file").and_then(Value::as_str) {
            Some(f) => ("sticky-note", file_title(f)),
            None => ("align-left", n.data.get("title").and_then(Value::as_str).unwrap_or("文本").to_string()),
        },
        "media" => (
            "image",
            n.data
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| n.data.get("file").and_then(Value::as_str))
                .map(file_title)
                .unwrap_or_else(|| "附件".to_string()),
        ),
        "table" => (
            "table",
            n.data
                .get("title")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .unwrap_or_else(|| n.data.get("file").and_then(Value::as_str).map(file_title).unwrap_or_default()),
        ),
        "link" => ("link", n.data.get("url").and_then(Value::as_str).unwrap_or("链接").to_string()),
        other => ("box", other.to_string()),
    }
}

/// 路径取文件名（去扩展名后缀的显示标题；根级与子目录一致）。
fn file_title(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

/// 非对话卡正文自绘（只读呈现）。
fn draw_card_body(
    session: &CanvasSession,
    view: &View,
    frame: &mut Frame,
    n: &atelyx_core::vault::CanvasFileNode,
    area: Rectangle,
) {
    let z = view.zoom;
    let size = pal::fs_caption() * z;
    match n.node_type.as_str() {
        // 对话卡正文 = 消息流浮层；无正文的文本卡按引用实况降级提示
        "conversation" => {}
        "text" => {
            if session.view.text_docs.contains_key(&n.id) {
                return;
            }
            let (hint, color) = match n.data.get("file").and_then(Value::as_str).map(|f| session.view.refs.get(f)) {
                Some(Some(RefContent::Missing)) => ("引用文件缺失", pal::DANGER),
                Some(Some(RefContent::Binary)) => ("二进制附件", pal::TEXT_MUTED),
                Some(Some(RefContent::Image { .. } | RefContent::Table { .. })) => ("引用内容非文本", pal::TEXT_MUTED),
                Some(None) => ("加载中…", pal::TEXT_MUTED),
                _ => ("（空）", pal::TEXT_MUTED),
            };
            draw_hint(frame, hint, area, size, color);
        }
        "media" => {
            let f = n.data.get("file").and_then(Value::as_str).unwrap_or("");
            let kind = n.data.get("kind").and_then(Value::as_str).unwrap_or("file");
            match session.view.refs.get(f) {
                // 图片按卡体等比 contain 绘制
                Some(RefContent::Image { handle, width, height }) if kind == "image" && *width > 0 && *height > 0 => {
                    let scale = (area.width / *width as f32).min(area.height / *height as f32).min(1.0);
                    let iw = *width as f32 * scale;
                    let ih = *height as f32 * scale;
                    let ix = area.x + (area.width - iw) / 2.0;
                    let iy = area.y + (area.height - ih) / 2.0;
                    frame.draw_image(
                        Rectangle::new(Point::new(ix, iy), Size::new(iw, ih)),
                        iced::advanced::image::Image::new(handle.clone()),
                    );
                }
                Some(RefContent::Image { .. }) if kind == "image" => {
                    draw_hint(frame, "图片加载中…", area, size, pal::TEXT_MUTED)
                }
                Some(RefContent::Text(body)) => draw_preview_lines(frame, body, area, size),
                Some(RefContent::Binary) => draw_hint(frame, "二进制附件", area, size, pal::TEXT_MUTED),
                Some(RefContent::Missing) => draw_hint(frame, "附件缺失", area, size, pal::DANGER),
                _ => draw_hint(frame, "加载中…", area, size, pal::TEXT_MUTED),
            }
        }
        "table" => {
            let f = n.data.get("file").and_then(Value::as_str).unwrap_or("");
            match session.view.refs.get(f) {
                Some(RefContent::Table { summary }) => {
                    draw_hint(frame, &format!("双击打开表格 · {summary}"), area, size, pal::TEXT_SECONDARY)
                }
                Some(RefContent::Missing) => draw_hint(frame, "引用表格缺失", area, size, pal::DANGER),
                _ => draw_hint(frame, "加载中…", area, size, pal::TEXT_MUTED),
            }
        }
        "link" => {
            let url = n.data.get("url").and_then(Value::as_str).unwrap_or("");
            draw_hint(frame, &layout_view::ellipsize(url, area.width, size), area, size, pal::ACCENT);
        }
        _ => draw_hint(frame, "未支持的卡类型（只读展示）", area, size, pal::TEXT_MUTED),
    }
}

/// 预览文本：按估宽折行 + 超出区域截断。
fn draw_preview_lines(frame: &mut Frame, body: &str, area: Rectangle, size: f32) {
    if body.trim().is_empty() {
        draw_hint(frame, "（空）", area, size, pal::TEXT_MUTED);
        return;
    }
    let line_h = size * 1.45;
    let max_lines = ((area.height / line_h) as usize).clamp(1, PREVIEW_LINES);
    let mut y = area.y;
    let mut drawn = 0;
    for src_line in body.lines() {
        if drawn >= max_lines {
            break;
        }
        if src_line.trim().is_empty() {
            // 空行同样占预算：否则一屏空行可把正文顶出卡体
            y += line_h * 0.5;
            drawn += 1;
            continue;
        }
        for seg in wrap_line(src_line, area.width, size) {
            if drawn >= max_lines {
                break;
            }
            frame.fill_text(CanvasText {
                content: seg,
                position: Point::new(area.x, y),
                max_width: f32::INFINITY,
                color: pal::TEXT_SECONDARY,
                size: size.into(),
                line_height: iced::advanced::text::LineHeight::Relative(1.45),
                font: crate::ui::regular(),
                align_x: iced::advanced::text::Alignment::Left,
                align_y: alignment::Vertical::Top,
                shaping: Shaping::Advanced,
            });
            y += line_h;
            drawn += 1;
        }
    }
}

/// 单行按估宽折行（超出 `max_w` 截断；返回一行或多行片段）。
fn wrap_line(line: &str, max_w: f32, size: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0.0f32;
    for c in line.chars() {
        let cw = layout_view::estimate_char_w(c, size);
        if w + cw > max_w && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            w = 0.0;
        }
        cur.push(c);
        w += cw;
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

fn draw_hint(frame: &mut Frame, hint: &str, area: Rectangle, size: f32, color: Color) {
    frame.fill_text(CanvasText {
        content: hint.to_string(),
        position: Point::new(area.x, area.y),
        max_width: f32::INFINITY,
        color,
        size: size.into(),
        line_height: iced::advanced::text::LineHeight::Relative(1.4),
        font: crate::ui::regular(),
        align_x: iced::advanced::text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
    });
}

/// 图标描边（lucide 24 视框缩放）。
fn draw_icon_glyph(frame: &mut Frame, name: &str, x: f32, y: f32, size: f32, color: Color) {
    let Some(subpaths) = polylines(name) else { return };
    let k = size / 24.0;
    frame.with_save(|f| {
        f.translate(Vector::new(x, y));
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
            f.stroke(
                &path,
                Stroke {
                    style: canvas::Style::Solid(color),
                    width: 2.0,
                    line_cap: iced::widget::canvas::LineCap::Round,
                    line_join: iced::widget::canvas::LineJoin::Round,
                    ..Stroke::default()
                },
            );
        }
    });
}

// ===== 视图组装 =====

/// 画布视图（工具条 + 世界层 + 对话卡消息流浮层）；未打开文件 = 空态。
/// 整体经 responsive 组装：工具条缩放钮与浮层锚点共用同一视口尺寸（视口高 = 总高 − 工具条高）。
pub fn canvas_view<'a>(app: &'a App, window: iced::window::Id) -> Element<'a> {
    let Some(session) = &app.canvas else {
        return controls::empty_state("palette", "未打开画布", "从文件面板打开一个 .atlx 文件", None);
    };

    responsive(move |size| {
        let viewport = Size::new(size.width.max(1.0), (size.height - TOOLBAR_H).max(1.0));
        // 适应视图待收口：按本帧视口尺寸即时渲染（与 commit_fit 同一纯函数，提交后同值不跳变）
        let view = if session.view.fit_pending {
            let (zoom, pan) = fit_transform(&session.data.nodes, |n| session.card_size(n), viewport.width, viewport.height);
            View { zoom, pan }
        } else {
            View::of(&session.view)
        };
        let world: Element<'_> = WorldLayer { session, view }.into();
        let mut layered = stack![world];
        for overlay in card_overlays(app, session, view, viewport) {
            layered = layered.push(overlay);
        }
        column![toolbar(session, window, viewport, view.zoom), layered.width(Length::Fill).height(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 工具条：统计 + 保存态 + 缩放钮（以视口中心为锚）；`zoom` 传本帧生效缩放（适应视图待收口时为适配值）。
fn toolbar<'a>(session: &'a CanvasSession, window: iced::window::Id, viewport: Size, zoom: f32) -> Element<'a> {
    let stats = format!("{} 卡 · {} 连线", session.data.nodes.len(), session.data.edges.len());
    let zoom_label = format!("{}%", (zoom * 100.0).round() as i32);
    let dirty_pill = if session.dirty {
        controls::status_pill(controls::PillStatus::Pending, "未保存")
    } else {
        space().width(Length::Shrink).into()
    };
    let save_state: Element<'_> = if let Some(err) = &session.save_error {
        text(layout_view::ellipsize(err, 220.0, pal::fs_micro()))
            .size(pal::fs_micro())
            .color(pal::DANGER)
            .into()
    } else {
        space().width(Length::Shrink).into()
    };
    let zoom_msg = |factor| Message::Canvas(CanvasMsg::Zoom { factor, size: viewport });

    row![
        space().width(Length::Fill),
        dirty_pill,
        save_state,
        controls::icon_button("maximize", 13.0, 24.0, controls::Variant::Ghost,
            Some(Message::Canvas(CanvasMsg::FitView)), Some((window, "适应视图".into()))),
        controls::icon_button("zoom-in", 13.0, 24.0, controls::Variant::Ghost,
            Some(zoom_msg(ZOOM_STEP)), Some((window, "放大".into()))),
        controls::icon_button("zoom-out", 13.0, 24.0, controls::Variant::Ghost,
            Some(zoom_msg(1.0 / ZOOM_STEP)), Some((window, "缩小".into()))),
        text(zoom_label).size(pal::fs_caption()).color(pal::TEXT_MUTED),
        text(stats).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
    ]
    .spacing(10)
    .padding(Padding::ZERO.horizontal(10.0))
    .height(Length::Fixed(TOOLBAR_H))
    .align_y(alignment::Vertical::Center)
    .into()
}

/// 卡体浮层：对话卡消息流与文本卡正文各自一个原生滚动区（锚点随视口换算）。
fn card_overlays<'a>(
    app: &'a App,
    session: &'a CanvasSession,
    view: View,
    viewport: Size,
) -> Vec<Element<'a>> {
    let (vw, vh) = (viewport.width, viewport.height);
    let mut out = Vec::with_capacity(session.data.nodes.len());
    for n in &session.data.nodes {
        let (w, h) = session.card_size(n);
        let (sx, sy) = view.screen(n.x, n.y);
        let (sw, sh) = (w as f32 * view.zoom, h as f32 * view.zoom);
        // 完全越出视口的卡不建正文；每张卡仍占一个槽位，Stack 按子级下标复用树状态，
        // 动态增删层会让后续卡的滚动位置串到别的卡上
        let offscreen = sx + sw < 0.0 || sy + sh < 0.0 || sx > vw || sy > vh;
        let inner: Option<Element<'a>> = if offscreen {
            None
        } else {
            match n.node_type.as_str() {
                "conversation" => n
                    .data
                    .get("messages")
                    .and_then(Value::as_array)
                    .filter(|messages| !messages.is_empty())
                    .map(|messages| {
                        // 消息流与面板同管线；展开态 / 解析缓存归画布视图状态（键为消息 id，与面板不撞）
                        let ctx = crate::chat_stream::StreamCtx {
                            docs: &session.view.docs,
                            images: &app.note_images,
                            expanded: &session.view.expanded,
                            theme: &app.theme,
                            scale: view.zoom,
                            actions: None,
                            streaming_id: None,
                            toggle: crate::chat_stream::ToggleRoute::Canvas,
                        };
                        crate::chat_stream::message_column(messages, &n.data, ctx)
                    }),
                "text" => session.view.text_docs.get(&n.id).map(|doc| {
                    // 正文复用笔记只读渲染，字号按视口缩放等比例（原生控件无整体缩放能力）
                    crate::markdown_view::document_body_scaled(
                        doc,
                        &app.theme,
                        &app.note_images,
                        app.global.soft_wrap,
                        view.zoom,
                    )
                }),
                _ => None,
            }
        };
        out.push(match inner {
            Some(inner) => {
                let body = scroll::v_scaled(
                    container(inner)
                        .width(Length::Fill)
                        .padding(Padding::ZERO.right(4.0 * view.zoom)),
                    view.zoom,
                )
                .width(Length::Fill)
                .height(Length::Fill);
                // 浮层自身不捕获按下：空白处按下落到画布层，由画布统一做命中与拖动
                // （正文里代码复制等按钮仍各自响应）
                place_card_overlay(body.into(), sx, sy, sw, sh, view.zoom)
            }
            None => space().width(Length::Shrink).height(Length::Shrink).into(),
        });
    }
    out
}

/// 把卡体浮层锚到卡的内容区：坐标与尺寸按世界坐标换算，可越出视口。
/// 下缘内缩让出选中卡右下角 resize 把手（把手只占角、不占整条右缘），滚动条贴卡片右缘。
fn place_card_overlay<'a>(
    inner: Element<'a>,
    sx: f32,
    sy: f32,
    sw: f32,
    sh: f32,
    zoom: f32,
) -> Element<'a> {
    // 卡内边距随视口缩放；把手让位为屏幕像素定值
    let pad = CARD_PAD * zoom;
    let bottom = pad.max(HANDLE_SIZE);
    let top = (CARD_HEADER_H + CARD_PAD) * zoom;
    ClipLayer::new(
        inner,
        Point::new(sx + pad, sy + top),
        Size::new((sw - pad).max(0.0), (sh - top - bottom).max(0.0)),
    )
    .into()
}

/// 越界裁剪层：内容按固定尺寸锚到自身坐标系中的任意位置（可为负），
/// 图层自身填满视口，绘制与指针命中都裁到图层边界。
struct ClipLayer<'a> {
    content: Element<'a>,
    origin: Point,
    content_size: Size,
}

impl<'a> ClipLayer<'a> {
    fn new(content: Element<'a>, origin: Point, content_size: Size) -> Self {
        Self { content, origin, content_size }
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for ClipLayer<'_> {
    fn tag(&self) -> widget::tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> widget::tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<widget::Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut widget::Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let mut node = self.content.as_widget_mut().layout(
            tree,
            renderer,
            &layout::Limits::new(Size::ZERO, self.content_size),
        );
        node.move_to_mut(self.origin);
        layout::Node::with_children(size, vec![node])
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content.as_widget_mut().operate(
            tree,
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // 视口外的指针不交给内容：越界部分不可见，不该截获滚轮与点击
        let cursor = if cursor.is_over(layout.bounds()) {
            cursor
        } else {
            cursor.levitate()
        };
        self.content.as_widget_mut().update(
            tree,
            event,
            layout.children().next().unwrap(),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if !cursor.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }
        self.content.as_widget().mouse_interaction(
            tree,
            layout.children().next().unwrap(),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Some(clip) = layout.bounds().intersection(viewport) else {
            return;
        };
        renderer.with_layer(clip, |renderer| {
            self.content.as_widget().draw(
                tree,
                renderer,
                theme,
                style,
                layout.children().next().unwrap(),
                cursor,
                &clip,
            );
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut widget::Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            tree,
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<ClipLayer<'a>> for Element<'a> {
    fn from(layer: ClipLayer<'a>) -> Self {
        Self::new(layer)
    }
}

// ===== 属性面板画布区 =====

/// 属性行（标签 + 值）。
fn props_row(label: &str, value: String) -> iced::Element<'static, Message> {
    row![
        text(label.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
        space().width(Length::Fill),
        text(value).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
    ]
    .spacing(8)
    .padding(Padding::ZERO.vertical(2.0))
    .into()
}

/// 画布属性面板：选中卡 = 坐标尺寸 / 类型 / 引用来源 / 消费关系；无选中 = 画布概况。只读展示。
pub fn props_panel(app: &App) -> iced::Element<'static, Message> {
    let Some(session) = &app.canvas else {
        return controls::view_placeholder("info", "画布属性", "打开一篇画布后在此查看其属性。");
    };
    let mut col = column![].spacing(4).padding(Padding::ZERO.horizontal(12.0).vertical(8.0));
    match session
        .view
        .selection
        .first()
        .and_then(|id| session.data.nodes.iter().find(|n| &n.id == id))
    {
        Some(n) => {
            let (w, h) = session.card_size(n);
            let (icon_name, title) = card_head(n);
            col = col.push(
                row![
                    icon(icon_name, 12.0, pal::TEXT_MUTED),
                    text(title).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
                ]
                .spacing(6),
            );
            col = col.push(props_row("类型", n.node_type.clone()));
            col = col.push(props_row("位置", format!("x {:.0} · y {:.0}", n.x, n.y)));
            col = col.push(props_row("尺寸", format!("{:.0} × {:.0}", w, h)));
            // 引用来源（笔记 / 媒体 / 表格卡的 file 字段）
            if let Some(f) = n.data.get("file").and_then(Value::as_str) {
                col = col.push(props_row("引用来源", f.to_string()));
                let state = match session.view.refs.get(f) {
                    Some(RefContent::Text(_)) | Some(RefContent::Image { .. }) | Some(RefContent::Table { .. }) => "已加载",
                    Some(RefContent::Binary) => "二进制",
                    Some(RefContent::Missing) => "缺失",
                    _ => "加载中",
                };
                col = col.push(props_row("引用状态", state.to_string()));
            }
            // 消费关系：该卡参与的连线（方向 + 对端 + 消费状态）
            let mut edges: Vec<String> = Vec::new();
            for e in &session.data.edges {
                let (self_is_source, other) = if e.source == n.id {
                    (true, e.target.as_str())
                } else if e.target == n.id {
                    (false, e.source.as_str())
                } else {
                    continue;
                };
                let Some(o) = session.data.nodes.iter().find(|x| x.id == other) else {
                    continue;
                };
                let (_, o_title) = card_head(o);
                let dir = if self_is_source { "→" } else { "←" };
                let tag = if is_dataflow(e) {
                    if edge_consumed(&session.data, e) { "数据流·已消费" } else { "数据流·未消费" }
                } else {
                    "关联"
                };
                edges.push(format!("{tag} {dir} {o_title}"));
            }
            if edges.is_empty() {
                col = col.push(props_row("连线", "无".to_string()));
            } else {
                for line in edges {
                    col = col.push(props_row("连线", line));
                }
            }
        }
        None => {
            let groups = session.data.nodes.iter().filter(|n| n.node_type == "group").count();
            col = col.push(props_row("标题", session.data.title.clone()));
            col = col.push(props_row("卡", format!("{}", session.data.nodes.len() - groups)));
            col = col.push(props_row("分组", groups.to_string()));
            col = col.push(props_row("连线", format!("{}", session.data.edges.len())));
            col = col.push(text("选中一张卡查看其属性").size(pal::fs_micro()).color(pal::TEXT_MUTED));
        }
    }
    iced::widget::container(col)
        .width(Length::Fill)
        .into()
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use atelyx_core::vault::{CanvasFile, CanvasFileEdge, CanvasFileNode};

    fn node(id: &str, node_type: &str, x: f64, y: f64, w: Option<f64>, h: Option<f64>) -> CanvasFileNode {
        CanvasFileNode { id: id.into(), node_type: node_type.into(), x, y, width: w, height: h, data: json!({}) }
    }

    fn edge(id: &str, source: &str, target: &str) -> CanvasFileEdge {
        CanvasFileEdge {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            source_handle: None,
            target_handle: None,
            directed: None,
            link_mode: None,
            created_at: 0,
        }
    }

    fn canvas(nodes: Vec<CanvasFileNode>, edges: Vec<CanvasFileEdge>) -> CanvasFile {
        CanvasFile {
            schema: "atelyx-canvas/v1".into(),
            id: "c1".into(),
            title: "t".into(),
            nodes,
            edges,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn world_screen_roundtrip() {
        let view = View { zoom: 1.7, pan: (-120.0, 55.0) };
        let (sx, sy) = view.screen(33.0, -4.5);
        let (wx, wy) = world_from_screen(&view, sx, sy);
        // f32 往返存在缩放乘除的表示误差，取 1e-4 容差
        assert!((wx - 33.0).abs() < 1e-4 && (wy + 4.5).abs() < 1e-4);
    }

    #[test]
    fn hit_prefers_topmost_card_over_group() {
        let c = canvas(
            vec![
                node("g", "group", 0.0, 0.0, Some(500.0), Some(500.0)),
                node("a", "text", 10.0, 10.0, Some(100.0), Some(100.0)),
                node("b", "text", 200.0, 200.0, Some(100.0), Some(100.0)),
            ],
            vec![],
        );
        // 组内卡命中 = 卡
        assert_eq!(hit_card(&c, (50.0, 50.0)).map(|i| c.nodes[i].id.as_str()), Some("a"));
        // 组内空白 = 组
        assert_eq!(hit_card(&c, (450.0, 450.0)).map(|i| c.nodes[i].id.as_str()), Some("g"));
        // 顶层优先：后声明的卡在上
        let c2 = canvas(
            vec![
                node("a", "text", 0.0, 0.0, Some(100.0), Some(100.0)),
                node("b", "text", 50.0, 50.0, Some(100.0), Some(100.0)),
            ],
            vec![],
        );
        assert_eq!(hit_card(&c2, (60.0, 60.0)).map(|i| c2.nodes[i].id.as_str()), Some("b"));
        // 空白 = None
        assert_eq!(hit_card(&c, (900.0, 900.0)), None);
    }

    #[test]
    fn resize_handle_hits_bottom_right_corner() {
        let c = canvas(vec![node("a", "text", 100.0, 100.0, Some(200.0), Some(150.0))], vec![]);
        let sel = vec!["a".to_string()];
        assert_eq!(hit_resize_handle(&c, &sel, 1.0, (300.0, 250.0)).as_deref(), Some("a"));
        assert_eq!(hit_resize_handle(&c, &sel, 1.0, (280.0, 240.0)), None);
        assert_eq!(hit_resize_handle(&c, &sel, 1.0, (200.0, 180.0)), None);
        // 未选中卡无把手（与「仅选中卡绘制把手」同口径）
        assert_eq!(hit_resize_handle(&c, &[], 1.0, (300.0, 250.0)), None);
    }

    #[test]
    fn edge_endpoints_fall_back_to_nearest_side() {
        // 水平相邻：源右缘 → 目标左缘
        let c = canvas(
            vec![
                node("a", "text", 0.0, 0.0, Some(100.0), Some(100.0)),
                node("b", "text", 300.0, 10.0, Some(100.0), Some(100.0)),
            ],
            vec![edge("e", "a", "b")],
        );
        let ((x0, y0), (x1, y1)) = edge_endpoints(&c, &c.edges[0]).unwrap();
        assert_eq!((x0, y0), (100.0, 50.0));
        assert_eq!((x1, y1), (300.0, 60.0));
    }

    #[test]
    fn handle_side_overrides_nearest() {
        let mut e = edge("e", "a", "b");
        e.source_handle = Some("top-source".into());
        e.target_handle = Some("left-target".into());
        let c = canvas(
            vec![
                node("a", "text", 0.0, 0.0, Some(100.0), Some(100.0)),
                node("b", "text", 300.0, 10.0, Some(100.0), Some(100.0)),
            ],
            vec![e],
        );
        let ((x0, y0), (x1, _)) = edge_endpoints(&c, &c.edges[0]).unwrap();
        assert_eq!((x0, y0), (50.0, 0.0));
        assert_eq!(x1, 300.0);
    }

    #[test]
    fn consumption_by_refs_node_id() {
        let mut conv = node("conv", "conversation", 300.0, 0.0, Some(200.0), Some(200.0));
        conv.data = json!({ "messages": [ { "id": "m1", "role": "user", "refs": [ { "nodeId": "src" } ] } ] });
        let c = canvas(
            vec![node("src", "text", 0.0, 0.0, Some(100.0), Some(100.0)), conv],
            vec![edge("e", "src", "conv")],
        );
        assert!(edge_consumed(&c, &c.edges[0]));
        // 无消息 = 未消费
        let mut conv2 = node("conv", "conversation", 300.0, 0.0, Some(200.0), Some(200.0));
        conv2.data = json!({ "messages": [] });
        let c2 = canvas(
            vec![node("src", "text", 0.0, 0.0, Some(100.0), Some(100.0)), conv2],
            vec![edge("e", "src", "conv")],
        );
        assert!(!edge_consumed(&c2, &c2.edges[0]));
        // 关联连线无消费语义
        let mut e2 = edge("e", "src", "conv");
        e2.directed = Some(false);
        let c3 = canvas(
            vec![
                node("src", "text", 0.0, 0.0, Some(100.0), Some(100.0)),
                node("conv", "conversation", 300.0, 0.0, Some(200.0), Some(200.0)),
            ],
            vec![e2],
        );
        assert!(!edge_consumed(&c3, &c3.edges[0]));
    }

    #[test]
    fn wrap_line_respects_width() {
        // 8 个 CJK 字符 ≈ 8 * 13.5 = 108px（12px 字号），限宽 60 → 两行
        let lines = wrap_line("一二三四五六七八", 60.0, 12.0);
        assert!(lines.len() >= 2);
        assert_eq!(lines.concat(), "一二三四五六七八");
    }

    #[test]
    fn fit_transform_centers_and_clamps() {
        let nodes = vec![node("a", "text", 0.0, 0.0, Some(200.0), Some(100.0))];
        let size = |n: &CanvasFileNode| node_size(n);
        let (zoom, pan) = fit_transform(&nodes, size, 800.0, 600.0);
        // 200 宽内容 + 两侧 FIT_PAD 装入 800 → zoom 钳在 1.0
        assert_eq!(zoom, 1.0);
        // 包围盒中心 (100, 50) 对准视口中心 (400, 300)
        let (sx, sy) = View { zoom, pan }.screen(100.0, 50.0);
        assert!((sx - 400.0).abs() < 1e-4 && (sy - 300.0).abs() < 1e-4);
        // 超大内容被缩小钳在下限之上
        let big = vec![node("a", "text", 0.0, 0.0, Some(20000.0), Some(10000.0))];
        let (zoom2, _) = fit_transform(&big, size, 800.0, 600.0);
        assert!((zoom2 - MIN_ZOOM).abs() < 1e-6 || zoom2 < 1.0);
    }
}
