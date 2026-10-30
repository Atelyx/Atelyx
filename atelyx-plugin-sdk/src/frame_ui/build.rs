//! 帧构建：容器先把子项收进来，再按最终矩形摆放并绘制。
//! 每个控件的绘制闭包在拿到自己的矩形后写指令、命中区与布局记录——输入路由只认布局记录。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use atelyx_plugin_protocol::paint::{DrawCommand, PaintColor};
use atelyx_plugin_protocol::frame::{FrameRect, HitCursor, HitRegion, SurfaceFrame};
use atelyx_plugin_protocol::method::MarkdownBlock;

use super::geom::Rect;
use super::metrics::Metrics;
use super::model::{FontSpec, Layout, UiState};
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
    /// 本帧用到但缓存里没有的 Markdown 原文：出帧后由驱动去取，取到即重建。
    pub markdown_misses: Vec<String>,
    /// 本帧用到的 Markdown 原文：缓存按它收敛，不随历史内容累积。
    pub markdown_used: HashSet<String>,
    /// 光标声明为拖动形态的子区：按下它们才建拖拽候选。
    pub draggables: HashSet<String>,
}

pub(crate) struct BuildOut {
    pub frame: SurfaceFrame,
    pub layouts: HashMap<String, Layout>,
    pub focusables: Vec<String>,
    pub scrolls: HashMap<String, f32>,
    pub scroll_max: HashMap<String, f32>,
    pub follow_prev: HashMap<String, bool>,
    pub markdown_misses: Vec<String>,
    pub markdown_used: HashSet<String>,
    pub metrics: Metrics,
    pub misses: bool,
    /// 光标声明为拖动形态的子区：按下它们才建拖拽候选。
    pub draggables: HashSet<String>,
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
            markdown_misses: Vec::new(),
            markdown_used: HashSet::new(),
            draggables: HashSet::new(),
        }
    }

    /// 取一段 Markdown 的解析块；缺项时记一次并按空块出这一帧，取到后重建。
    pub(crate) fn markdown_blocks(&mut self, content: &str) -> Vec<MarkdownBlock> {
        self.markdown_used.insert(content.to_string());
        match self.state.markdown.get(content) {
            Some(blocks) => blocks.clone(),
            None => {
                if !self.markdown_misses.iter().any(|miss| miss == content) {
                    self.markdown_misses.push(content.to_string());
                }
                Vec::new()
            }
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

    /// 汇总成帧：键盘声明按「本帧有无可聚焦控件」定，输入法光标取聚焦输入框的光标矩形。
    pub(crate) fn finish(self) -> BuildOut {
        let keys = !self.focusables.is_empty();
        let ime = self.ime.map(|r| FrameRect { x: r.x, y: r.y, w: r.w, h: r.h });
        let misses = self.metrics.has_misses();
        BuildOut {
            frame: SurfaceFrame { commands: self.commands, hits: self.hits, ime, keys, fps: None },
            layouts: self.layouts,
            focusables: self.focusables,
            scrolls: self.scrolls,
            scroll_max: self.scroll_max,
            follow_prev: self.follow_prev,
            markdown_misses: self.markdown_misses,
            markdown_used: self.markdown_used,
            metrics: self.metrics,
            misses,
            draggables: self.draggables,
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
            max_width: None,
            align: None,
        });
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

    /// 纵向容器：子项按声明序自上而下，`gap` 为间距。
    pub fn column(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        self.items.push(column_item(sub.items, gap));
    }

    /// 横向容器：收缩子项按内容宽，撑满子项均分剩余宽度，子项在行内垂直居中。
    pub fn row(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        self.items.push(row_item(sub.items, gap, false));
    }

    /// 占满剩余高度的横向容器：子项若声明吃满高度（滚动区）即拿到整行高度，用于并排分栏各自滚动。
    pub fn row_fill(&mut self, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let mut sub = Ui { ctx: &mut *self.ctx, clip: self.clip, items: Vec::new() };
        build(&mut sub);
        self.items.push(row_item(sub.items, gap, true));
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
    let mut fixed: f32 = 0.0;
    let mut fill_count = 0usize;
    for item in items {
        match &item.width {
            Width::Fixed(w) => fixed += w,
            Width::Fit(measure) => fixed += measure(metrics),
            Width::Fill => fill_count += 1,
        }
    }
    let gaps = gap * items.len().saturating_sub(1) as f32;
    let share = if fill_count > 0 { ((width - fixed - gaps).max(0.0)) / fill_count as f32 } else { 0.0 };
    let mut widths = Vec::with_capacity(items.len());
    for item in items {
        widths.push(match &item.width {
            Width::Fixed(w) => *w,
            Width::Fit(measure) => measure(metrics),
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
