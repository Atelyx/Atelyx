//! 帧构建：容器先把子项收进来，再按最终矩形摆放并绘制。
//! 每个控件的绘制闭包在拿到自己的矩形后写指令、命中区与布局记录——输入路由只认布局记录。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::rc::Rc;

use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor};
use atelyx_plugin_protocol::frame::{FrameRect, HitCursor, HitRegion, SurfaceFrame};

use super::geom::Rect;
use super::metrics::Metrics;
use super::model::{FontSpec, Layout, UiState};
use super::theme;

/// 提示标签的内边距（左右 / 上下）。
const HINT_PAD_X: f32 = 8.0;
const HINT_PAD_Y: f32 = 4.0;
use super::theme::Theme;

/// 一帧的绘制上下文：指令 / 命中区 / 布局记录与量算都在这里，绘制结束后整批交回。
pub(crate) struct BuildCtx<'a> {
    commands: Vec<DrawCommand>,
    hits: Vec<HitRegion>,
    pub metrics: Metrics,
    pub theme: &'a Theme,
    pub state: &'a UiState,
    pub layouts: HashMap<String, Layout>,
    pub focusables: Vec<String>,
    pub ime: Option<Rect>,
    pub scrolls: HashMap<String, f32>,
    pub scroll_max: HashMap<String, f32>,
    pub follow_prev: HashMap<String, bool>,
    /// 光标声明为拖动形态的子区：按下它们才建拖拽候选。
    pub draggables: HashSet<String>,
    /// 浮层：主内容布局完成后按声明序绘制，指令与命中区都压在主内容之上；
    /// 布尔 = 是否拦截滚轮（弹层拦截，角标 / 浮钮这类被动浮层不拦）。
    pub overlays: Vec<(Rect, Vec<Item>, bool)>,
    /// 已布局的浮层区域（随帧交给状态，供滚轮隔离判定）。
    pub overlay_rects: Vec<Rect>,
    /// 正在绘制浮层：滚动容器据此记入 `overlay_scrolls`（滚轮隔离）。
    in_overlay: bool,
    /// 浮层内的滚动容器 id。
    pub overlay_scrolls: HashSet<String>,
    /// 本帧画了靠帧拍推进的动画（自转指示）：驱动据此声明帧拍。
    pub wants_ticks: bool,
    /// 本帧内 Markdown 块的序号：块 id 与代码块复制钮的命中都按它区分（插件看不到）。
    markdown_seq: u32,
    /// 本帧声明的悬停提示（控件 id → 文字）：随帧交给状态，下一帧的输入路由按它出提示。
    hints: HashMap<String, String>,
    /// 本帧画过的只读正文版面：状态留一份，选择命中与复制按它算。
    pub markdown_seen: Vec<(String, Arc<super::markdown::Layout>)>,
}

pub(crate) struct BuildOut {
    pub frame: SurfaceFrame,
    pub layouts: HashMap<String, Layout>,
    pub focusables: Vec<String>,
    pub scrolls: HashMap<String, f32>,
    pub scroll_max: HashMap<String, f32>,
    pub follow_prev: HashMap<String, bool>,
    pub metrics: Metrics,
    pub misses: bool,
    /// 本帧画了自转指示这类靠帧拍推进的动画：驱动据此声明帧拍。
    pub wants_ticks: bool,
    /// 本帧声明的悬停提示（控件 id → 文字）：驱动写回状态，供下一帧的输入路由使用。
    pub hints: HashMap<String, String>,
    /// 本帧画过的只读正文版面。
    pub markdown_seen: Vec<(String, Arc<super::markdown::Layout>)>,
    /// 光标声明为拖动形态的子区：按下它们才建拖拽候选。
    pub draggables: HashSet<String>,
    pub overlays: Vec<Rect>,
    pub overlay_scrolls: HashSet<String>,
}

impl<'a> BuildCtx<'a> {
    pub(crate) fn new(theme: &'a Theme, metrics: Metrics, state: &'a UiState) -> Self {
        Self {
            commands: Vec::new(),
            hits: Vec::new(),
            metrics,
            theme,
            state,
            layouts: HashMap::new(),
            focusables: Vec::new(),
            ime: None,
            scrolls: state.scroll.clone(),
            scroll_max: state.scroll_max.clone(),
            follow_prev: state.follow_prev.clone(),
            draggables: HashSet::new(),
            overlays: Vec::new(),
            overlay_rects: Vec::new(),
            in_overlay: false,
            overlay_scrolls: HashSet::new(),
            wants_ticks: false,
            markdown_seq: 0,
            hints: HashMap::new(),
            markdown_seen: Vec::new(),
        }
    }

    pub(crate) fn draw(&mut self, command: DrawCommand) {
        self.commands.push(command);
    }

    pub(crate) fn hit(&mut self, id: &str, rect: Rect, cursor: HitCursor) {
        self.hits.push(HitRegion { id: id.to_string(), x: rect.x, y: rect.y, w: rect.w, h: rect.h, cursor: Some(cursor) });
        if matches!(
            cursor,
            HitCursor::Grab | HitCursor::Grabbing | HitCursor::ResizeHorizontal | HitCursor::ResizeVertical
        ) {
            self.draggables.insert(id.to_string());
        }
    }

    pub(crate) fn record(&mut self, id: &str, layout: Layout) {
        if self.in_overlay && matches!(layout, Layout::Scroll { .. }) {
            self.overlay_scrolls.insert(id.to_string());
        }
        self.layouts.insert(id.to_string(), layout);
    }

    pub(crate) fn hovered(&self, id: &str) -> bool {
        self.state.hovered.as_deref() == Some(id)
    }

    pub(crate) fn pressed(&self, id: &str) -> bool {
        self.state.press.as_ref().is_some_and(|press| press.id == id)
    }

    pub(crate) fn focused(&self, id: &str) -> bool {
        self.state.focus.as_deref() == Some(id)
    }

    /// 本帧一个 Markdown 块的内部身份：同帧多块互不冲突，插件无需命名。
    /// 带上内容指纹：块序因新消息插入而错位时，选择不会悄悄落到另一段文字上。
    pub(crate) fn next_markdown_id(&mut self, content: u64) -> String {
        self.markdown_seq += 1;
        format!("@md:{}:{content:x}", self.markdown_seq)
    }

    /// 光标此刻是否显形（静默时半秒亮、半秒灭）。
    pub(crate) fn caret_lit(&self) -> bool {
        self.state.caret_lit()
    }

    /// 声明一个控件的悬停提示：指针停够时间后由控件层画在指针下沿（插件不必自己管浮层）。
    pub(crate) fn declare_hint(&mut self, id: &str, text: &str) {
        if !text.is_empty() {
            self.hints.insert(id.to_string(), text.to_string());
        }
    }

    /// 提示标签：悬停够久后压在最后画（整块留在面内，贴边时向里收）。
    pub(crate) fn paint_hint(&mut self) {
        let Some(label) = self.state.hint_label() else {
            return;
        };
        let Some((pointer_x, pointer_y)) = self.state.pointer else {
            return;
        };
        let spec = FontSpec { size: self.theme.sizes.micro, weight: None, family: None };
        let line = self.metrics.measure("", spec, None).height.max(spec.size * 1.2);
        let width = self.metrics.width(&label, spec) + HINT_PAD_X * 2.0;
        let height = line + HINT_PAD_Y * 2.0;
        let size = self.state.size;
        let x = (pointer_x + 10.0).clamp(4.0, (size.0 - width - 4.0).max(4.0));
        let y = (pointer_y + 18.0).clamp(4.0, (size.1 - height - 4.0).max(4.0));
        self.draw(DrawCommand::Rect {
            x,
            y,
            w: width,
            h: height,
            fill: Some(theme::token(ColorToken::Surface)),
            stroke: Some(theme::token(ColorToken::Border)),
            stroke_width: Some(1.0),
            corner_radius: Some(4.0),
            shadow: None,
        });
        self.draw(DrawCommand::Text {
            x: x + HINT_PAD_X,
            y: y + HINT_PAD_Y,
            content: label,
            size: spec.size,
            color: theme::token(ColorToken::TextSecondary),
            weight: None,
            family: None,
            family_name: None,
            italic: false,
            max_width: None,
            align: None,
            layout: None,
        });
    }

    /// 汇总成帧：键盘声明按「本帧有无可聚焦控件」定，输入法光标取聚焦输入框的光标矩形。
    pub(crate) fn finish(self) -> BuildOut {
        let keys = !self.focusables.is_empty();
        let ime = self.ime.map(|r| FrameRect { x: r.x, y: r.y, w: r.w, h: r.h });
        let misses = self.metrics.has_misses();
        let hints = self.hints;
        BuildOut {
            hints,
            markdown_seen: self.markdown_seen,
            frame: SurfaceFrame {
                size: None,
                commands: self.commands,
                hits: self.hits,
                ime,
                keys,
                fps: None,
                popup: None,
            },
            layouts: self.layouts,
            focusables: self.focusables,
            scrolls: self.scrolls,
            scroll_max: self.scroll_max,
            follow_prev: self.follow_prev,
            metrics: self.metrics,
            misses,
            wants_ticks: self.wants_ticks,
            draggables: self.draggables,
            overlays: self.overlay_rects,
            overlay_scrolls: self.overlay_scrolls,
        }
    }
}

/// 自绘画布：区域与裁剪由容器给定，闭包里画指令并登记子区命中。
/// 指令集没有裁剪，超出 `visible()` 的内容由闭包自己收住；子区命中按声明逆序取最上面的一个。
pub struct Canvas<'c, 'a> {
    ctx: &'c mut BuildCtx<'a>,
    rect: Rect,
    clip: Rect,
}

impl<'c, 'a> Canvas<'c, 'a> {
    pub(crate) fn new(ctx: &'c mut BuildCtx<'a>, rect: Rect, clip: Rect) -> Self {
        Self { ctx, rect, clip }
    }

    /// 控件在本帧拿到的完整矩形。
    pub fn area(&self) -> Rect {
        self.rect
    }

    /// 与容器裁剪相交后的可见矩形；为空时闭包不会被执行。
    pub fn visible(&self) -> Rect {
        self.rect.intersect(self.clip)
    }

    pub fn draw(&mut self, command: DrawCommand) {
        self.ctx.draw(command);
    }

    /// 登记可点击子区：命中时上报 `Click` / `DoubleClick` / `RightClick`；光标为 `Grab` /
    /// `Grabbing` / 单向缩放时该子区还可拖动——位移越过起拖阈值后改报 `DragStart` / `Drag` /
    /// `DragEnd`（此后的抬起不再算点击）。`Pointer` 这类普通光标下的小抖动仍按点击解释。
    /// 子区裁到可见范围内：滚出视口的部分不该继续吃点击。
    pub fn hit(&mut self, id: &str, area: Rect, cursor: HitCursor) {
        let area = area.intersect(self.visible());
        if area.is_empty() {
            return;
        }
        self.ctx.hit(id, area, cursor);
        self.ctx.record(id, Layout::Custom);
    }

    pub fn hovered(&self, id: &str) -> bool {
        self.ctx.hovered(id)
    }

    pub fn pressed(&self, id: &str) -> bool {
        self.ctx.pressed(id)
    }

    pub fn focused(&self, id: &str) -> bool {
        self.ctx.focused(id)
    }

    /// 单行文字的宽高：缺项先按估算值排版，量算落地后自动重建。
    pub fn measure(&mut self, text: &str, size: f32) -> (f32, f32) {
        let result = self.ctx.metrics.measure(text, FontSpec { size, weight: None, family: None }, None);
        (result.width, result.height)
    }

    /// 一段单行文字，`(x, y)` 为左上角。
    pub fn text(&mut self, content: impl Into<String>, x: f32, y: f32, size: f32, color: PaintColor) {
        self.ctx.draw(DrawCommand::Text {
            x,
            y,
            content: content.into(),
            size,
            color,
            weight: None,
            family: None,
            family_name: None,
            italic: false,
            max_width: None,
            align: None,
            layout: None,
        });
    }

    /// 内置图标：`rect` 是图标的完整方框（形状按方框宽 / 24 缩放），传裁剪后的可见矩形会缩小图形。
    pub fn icon(&mut self, icon: super::icons::Icon, rect: Rect, color: PaintColor) {
        super::icons::draw(self.ctx, icon, rect, color);
    }
}

/// 子项宽度声明：固定宽、撑满（父容器给多少算多少）、按内容收缩。
pub(crate) enum Width {
    Fixed(f32),
    Fill,
    Fit(Box<dyn Fn(&mut Metrics) -> f32>),
}

pub(crate) type HeightFn = Box<dyn Fn(&mut Metrics, f32) -> f32>;
pub(crate) type DrawFn = Box<dyn Fn(&mut BuildCtx<'_>, Rect, Rect)>;

/// 容器里的一个子项。
pub(crate) struct Item {
    pub width: Width,
    /// 高由父容器剩余空间决定（滚动容器这类需要视口的控件）。
    pub fill_height: bool,
    /// 给定最终宽度后的固有高。
    pub height: HeightFn,
    /// 给定最终矩形与裁剪矩形后的绘制。
    pub draw: DrawFn,
}

/// 帧构建句柄（交给插件的声明面）：只在一次构建内有效。
pub struct Ui<'c, 'a> {
    pub(crate) ctx: &'c mut BuildCtx<'a>,
    pub(crate) clip: Rect,
    items: Vec<Item>,
}

impl<'c, 'a> Ui<'c, 'a> {
    pub(crate) fn new(ctx: &'c mut BuildCtx<'a>, clip: Rect) -> Self {
        Self { ctx, clip, items: Vec::new() }
    }

    pub(crate) fn into_items(self) -> Vec<Item> {
        self.items
    }

    pub(crate) fn push(&mut self, item: Item) {
        self.items.push(item);
    }

    pub(crate) fn ctx(&mut self) -> &mut BuildCtx<'a> {
        self.ctx
    }

    /// 某控件本帧是否被悬停（自绘外观按状态变化时读）。
    pub fn hovered(&self, id: &str) -> bool {
        self.ctx.hovered(id)
    }

    /// 某控件本帧是否被按下。
    pub fn pressed(&self, id: &str) -> bool {
        self.ctx.pressed(id)
    }

    /// 某控件本帧是否持有焦点。
    pub fn focused(&self, id: &str) -> bool {
        self.ctx.focused(id)
    }

    /// 本面的完整矩形（浮层定位用；坐标为面本地逻辑像素）。
    pub fn bounds(&self) -> Rect {
        self.clip
    }

    /// 纵向容器：子项按声明序自上而下，`gap` 为间距。
    pub fn column(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        self.items.push(column_item(sub.items, gap));
    }

    /// 水平居中的纵向容器（空态、提示块）。
    pub fn center_column(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui::new(&mut *self.ctx, self.clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |metrics, width| column_height(&for_size, metrics, width, gap)),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let mut y = rect.y;
                for item in shared.iter() {
                    let width = child_width(item, &mut ctx.metrics, rect.w);
                    let height = child_height(item, &mut ctx.metrics, width);
                    let child = Rect::new(rect.x + (rect.w - width) / 2.0, y, width, height);
                    let child_visible = child.intersect(visible);
                    if !child_visible.is_empty() {
                        (item.draw)(ctx, child, child_visible);
                    }
                    y += height + gap;
                }
            }),
        });
    }

    /// 限宽纵向容器：宽度取「可用宽」与 `max_width` 的较小值并水平居中（`max_width <= 0` = 不限）。
    /// 正文列的行宽偏好用它；子项按列排布。
    pub fn bounded(&mut self, max_width: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let fill_height = shared.iter().any(|item| item.fill_height);
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height,
            height: Box::new(move |metrics, width| {
                let bound = if max_width > 0.0 { width.min(max_width) } else { width };
                column_height(&for_size, metrics, bound, 0.0)
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let bound = if max_width > 0.0 { rect.w.min(max_width) } else { rect.w };
                let inner = Rect::new(rect.x + (rect.w - bound) / 2.0, rect.y, bound, rect.h);
                let visible = inner.intersect(clip);
                if !visible.is_empty() {
                    draw_column(ctx, &shared, inner, visible, 0.0);
                }
            }),
        });
    }

    /// 撑满面的纵向容器：根节点按面高收住，内部的滚动区拿剩余高度（整页布局用）。
    pub fn fill_column(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        let mut item = column_item(sub.items, gap);
        item.fill_height = true;
        self.items.push(item);
    }

    /// 定宽侧栏：在撑满高度的行里占满高，内部撑满子项（滚动区）拿剩余高度。
    pub fn side(&mut self, width: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        let mut item = column_item(sub.items, 0.0);
        item.width = Width::Fixed(width);
        item.fill_height = true;
        self.items.push(item);
    }

    /// 横向容器：收缩子项按内容宽，撑满子项均分剩余宽度，子项在行内垂直居中。
    pub fn row(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        self.items.push(row_item(sub.items, gap, false));
    }

    /// 收缩宽度的横向容器：宽度由子项决定（行内右对齐按钮组）。
    pub fn row_fit(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui::new(&mut *self.ctx, self.clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_width = Rc::clone(&shared);
        let for_height = Rc::clone(&shared);
        let for_draw = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fit(Box::new(move |metrics| {
                let slots = row_slots(&for_width, metrics, f32::MAX / 4.0, gap);
                slots.widths.iter().sum::<f32>() + gap * for_width.len().saturating_sub(1) as f32
            })),
            fill_height: false,
            height: Box::new(move |metrics, width| {
                row_slots(&for_height, metrics, width, gap).total_height
            }),
            draw: Box::new(move |ctx, rect, clip| draw_row(ctx, &for_draw, rect, clip, gap)),
        });
    }

    /// 占满剩余高度的横向容器：子项若声明吃满高度（滚动区）即拿到整行高度，用于并排分栏各自滚动。
    pub fn row_fill(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        self.items.push(row_item(sub.items, gap, true));
    }

    /// 层叠容器：子项都从左上角铺开，按声明序绘制（后声明压住先声明，命中同样后优先）。
    pub fn stack(&mut self, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui::new(&mut *self.ctx, self.clip);
        build(&mut sub);
        self.items.push(stack_item(sub.items));
    }

    /// 浮层：内容按纵向容器排在 `rect` 内，主内容布局完成后整体追加绘制与命中区。
    /// 整块登记点击兜底（子项命中优先），空白处不穿透到下层；有浮层时滚轮只作用于浮层内的滚动区。
    pub fn overlay(&mut self, rect: Rect, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.overlay_kind(rect, true, build);
    }

    /// 被动浮层：绘制与命中同 [`Ui::overlay`]，但不拦截滚轮（浮钮 / 角标这类不应挡住下层滚动）。
    pub fn overlay_passive(&mut self, rect: Rect, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.overlay_kind(rect, false, build);
    }

    fn overlay_kind(&mut self, rect: Rect, blocks_wheel: bool, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui::new(&mut *self.ctx, rect);
        build(&mut sub);
        let items = sub.into_items();
        self.ctx.overlays.push((rect, items, blocks_wheel));
    }
}

/// 根布局：单个根容器高过面时按面高收住——撑满子项（滚动区）拿面的剩余高度，
/// 而不是按内容高铺开（否则内容一超屏，滚动区就没有视口）。较矮的根子项保持自身高度。
pub(crate) fn layout_root(ctx: &mut BuildCtx<'_>, mut items: Vec<Item>, rect: Rect) {
    if items.len() == 1 {
        let intrinsic = child_height(&items[0], &mut ctx.metrics, rect.w);
        if intrinsic >= rect.h {
            items[0].fill_height = true;
        }
    }
    draw_column(ctx, &items, rect, rect, 0.0);
}

/// 浮层布局：按声明序叠在主内容之后，绘制顺序与命中优先级都压在其上。
pub(crate) fn layout_overlays(ctx: &mut BuildCtx<'_>) {
    let overlays = std::mem::take(&mut ctx.overlays);
    ctx.in_overlay = true;
    for (index, (rect, items, blocks_wheel)) in overlays.into_iter().enumerate() {
        if blocks_wheel {
            ctx.overlay_rects.push(rect);
        }
        // 浮层整块承接点击：子项随后登记，命中优先于这块兜底
        ctx.hit(&format!("@overlay:{index}"), rect, HitCursor::Default);
        draw_column(ctx, &items, rect, rect, 0.0);
    }
    ctx.in_overlay = false;
}

pub(crate) fn child_width(item: &Item, metrics: &mut Metrics, available: f32) -> f32 {
    match &item.width {
        Width::Fixed(width) => width.min(available),
        Width::Fill => available,
        Width::Fit(measure) => measure(metrics).min(available),
    }
}

pub(crate) fn child_height(item: &Item, metrics: &mut Metrics, width: f32) -> f32 {
    (item.height)(metrics, width)
}

fn stack_item(items: Vec<Item>) -> Item {
    let shared = Rc::new(items);
    let for_size = Rc::clone(&shared);
    Item {
        width: Width::Fill,
        fill_height: true,
        height: Box::new(move |metrics, width| {
            for_size
                .iter()
                .map(|item| child_height(item, metrics, width))
                .fold(0.0_f32, f32::max)
        }),
        draw: Box::new(move |ctx, rect, clip| draw_stack(ctx, &shared, rect, clip)),
    }
}

fn draw_stack(ctx: &mut BuildCtx<'_>, items: &[Item], rect: Rect, clip: Rect) {
    for item in items {
        let width = child_width(item, &mut ctx.metrics, rect.w);
        let height = if item.fill_height {
            rect.h
        } else {
            child_height(item, &mut ctx.metrics, width).min(rect.h)
        };
        let child = Rect::new(rect.x, rect.y, width, height);
        let visible = child.intersect(clip);
        if !visible.is_empty() {
            (item.draw)(ctx, child, visible);
        }
    }
}

fn column_item(items: Vec<Item>, gap: f32) -> Item {
    let shared = Rc::new(items);
    let for_size = Rc::clone(&shared);
    Item {
        width: Width::Fill,
        fill_height: false,
        height: Box::new(move |metrics, width| column_height(&for_size, metrics, width, gap)),
        draw: Box::new(move |ctx, rect, clip| draw_column(ctx, &shared, rect, clip, gap)),
    }
}

pub(crate) fn column_height(items: &[Item], metrics: &mut Metrics, width: f32, gap: f32) -> f32 {
    let mut total = 0.0;
    for item in items {
        let w = child_width(item, metrics, width);
        total += child_height(item, metrics, w);
    }
    total + gap * items.len().saturating_sub(1) as f32
}

pub(crate) fn draw_column(ctx: &mut BuildCtx<'_>, items: &[Item], rect: Rect, clip: Rect, gap: f32) {
    let widths: Vec<f32> = items.iter().map(|item| child_width(item, &mut ctx.metrics, rect.w)).collect();
    let heights: Vec<f32> = items
        .iter()
        .zip(&widths)
        .map(|(item, width)| child_height(item, &mut ctx.metrics, *width))
        .collect();
    let gaps = gap * items.len().saturating_sub(1) as f32;
    let fill_count = items.iter().filter(|item| item.fill_height).count();
    // 撑满子项不吃「固有高」总额：它们的固有高只是内容高，视口由父容器剩余空间给
    let occupied: f32 = items
        .iter()
        .zip(&heights)
        .filter(|(item, _)| !item.fill_height)
        .map(|(_, height)| *height)
        .sum::<f32>()
        + gaps;
    let share = if fill_count > 0 { ((rect.h - occupied).max(0.0)) / fill_count as f32 } else { 0.0 };
    let mut y = rect.y;
    for (index, item) in items.iter().enumerate() {
        let height = if item.fill_height && share > 0.0 { share } else { heights[index] };
        let child = Rect::new(rect.x, y, widths[index], height);
        let visible = child.intersect(clip);
        if !visible.is_empty() {
            (item.draw)(ctx, child, visible);
        }
        y += height + gap;
    }
}

fn row_item(items: Vec<Item>, gap: f32, fill_height: bool) -> Item {
    let shared = Rc::new(items);
    let for_size = Rc::clone(&shared);
    Item {
        width: Width::Fill,
        fill_height,
        height: Box::new(move |metrics, width| {
            let slots = row_slots(&for_size, metrics, width, gap);
            slots.total_height
        }),
        draw: Box::new(move |ctx, rect, clip| draw_row(ctx, &shared, rect, clip, gap)),
    }
}

struct RowSlots {
    widths: Vec<f32>,
    heights: Vec<f32>,
    total_height: f32,
}

/// 行内槽位：固定 / 收缩子项先占宽，撑满子项均分剩余；行高取子项固有高的最大值。
fn row_slots(items: &[Item], metrics: &mut Metrics, width: f32, gap: f32) -> RowSlots {
    let gaps = gap * items.len().saturating_sub(1) as f32;
    let available = (width - gaps).max(0.0);
    let mut fixed: f32 = 0.0;
    let mut fill_count = 0usize;
    for item in items {
        match &item.width {
            Width::Fixed(w) => fixed += (*w).min(available),
            Width::Fit(measure) => fixed += measure(metrics).min(available),
            Width::Fill => fill_count += 1,
        }
    }
    let share = if fill_count > 0 { (width - fixed - gaps).max(0.0) / fill_count as f32 } else { 0.0 };
    let mut widths = Vec::with_capacity(items.len());
    for item in items {
        widths.push(match &item.width {
            Width::Fixed(w) => (*w).min(available),
            Width::Fit(measure) => measure(metrics).min(available),
            Width::Fill => share,
        });
    }
    let heights: Vec<f32> = items
        .iter()
        .zip(&widths)
        .map(|(item, w)| child_height(item, metrics, *w))
        .collect();
    let total_height = heights.iter().fold(0.0_f32, |a, b| a.max(*b));
    RowSlots { widths, heights, total_height }
}

fn draw_row(ctx: &mut BuildCtx<'_>, items: &[Item], rect: Rect, clip: Rect, gap: f32) {
    let slots = row_slots(items, &mut ctx.metrics, rect.w, gap);
    let mut x = rect.x;
    for (index, item) in items.iter().enumerate() {
        // 吃满高度的子项（滚动区）拿走整行高度，否则按内容高并在行内垂直居中
        let height = if item.fill_height { rect.h } else { slots.heights[index].min(rect.h) };
        let child = Rect::new(x, rect.y + (rect.h - height) / 2.0, slots.widths[index], height);
        let visible = child.intersect(clip);
        if !visible.is_empty() {
            (item.draw)(ctx, child, visible);
        }
        x += slots.widths[index] + gap;
    }
}
