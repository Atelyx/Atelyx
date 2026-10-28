//! SDK 控件：文字、按钮、输入框、下拉、勾选与滚动容器。
//! 每个控件在自己的绘制闭包里写指令、命中区与布局记录；外观取主题 token，尺寸与宿主表单控件同档。

use std::rc::Rc;

use atelyx_plugin_protocol::canvas::{CanvasPoint, DrawCommand, TextAlign};
use atelyx_plugin_protocol::frame::HitCursor;
use atelyx_plugin_protocol::view::{ColorToken, TextWeight};

use super::build::{child_height, child_width, column_height, BuildCtx, Item, Ui, Width};
use super::geom::Rect;
use super::model::{FontSpec, InputState, Layout};
use super::text;
use super::theme::{self, Theme, BUTTON_H, BUTTON_PAD_X, CHECK_BOX, INPUT_H, OPTION_GAP, OPTION_H, PAD_X};

/// 文字角色（字号 / 字重 / 颜色一并由主题决定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRole {
    /// 区块标题。
    Heading,
    /// 正文说明。
    Label,
    /// 字段标签。
    FieldLabel,
    /// 小号说明（可换行）。
    Caption,
    /// 表单值。
    Value,
}

impl TextRole {
    fn resolve(self, theme: &Theme) -> (FontSpec, ColorToken) {
        let sizes = &theme.sizes;
        match self {
            TextRole::Heading => {
                (FontSpec { size: sizes.body, weight: Some(TextWeight::Medium), family: None }, ColorToken::Text)
            }
            TextRole::Label => (FontSpec { size: sizes.ui, weight: None, family: None }, ColorToken::TextSecondary),
            TextRole::FieldLabel => (
                FontSpec { size: sizes.caption, weight: Some(TextWeight::Medium), family: None },
                ColorToken::TextSecondary,
            ),
            TextRole::Caption => (FontSpec { size: sizes.caption, weight: None, family: None }, ColorToken::TextMuted),
            TextRole::Value => (FontSpec { size: sizes.ui, weight: None, family: None }, ColorToken::Text),
        }
    }
}

impl<'c, 'a> Ui<'c, 'a> {
    /// 单行文字（占满父宽，超出按宽度省略）。
    pub fn text(&mut self, content: impl Into<String>, role: TextRole) {
        let content = content.into();
        let (spec, color) = role.resolve(self.ctx().theme);
        self.push(text_item(content, spec, color, false));
    }

    /// 小号说明：占满宽度并按宽度换行。
    pub fn caption(&mut self, content: impl Into<String>) {
        let content = content.into();
        let (spec, color) = TextRole::Caption.resolve(self.ctx().theme);
        self.push(text_item(content, spec, color, true));
    }

    /// 字段标签：可换行（长说明按可用宽度折行，不截断）。
    pub fn field_label(&mut self, content: impl Into<String>) {
        let content = content.into();
        let (spec, color) = TextRole::FieldLabel.resolve(self.ctx().theme);
        self.push(text_item(content, spec, color, true));
    }

    /// 分隔线。
    pub fn rule(&mut self) {
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(|_, _| 1.0),
            draw: Box::new(|ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                ctx.draw(DrawCommand::Rect {
                    x: visible.x,
                    y: visible.y,
                    w: visible.w,
                    h: visible.h,
                    fill: Some(theme::token(ColorToken::Border)),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: None,
                });
            }),
        });
    }

    /// 固定高度的空白。
    pub fn spacer(&mut self, height: f32) {
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |_, _| height),
            draw: Box::new(|_, _, _| {}),
        });
    }

    /// 常规按钮（次级底）。
    pub fn button(&mut self, id: impl Into<String>, label: impl Into<String>) {
        self.button_kind(id.into(), label.into(), false);
    }

    /// 幽灵按钮（无底色，悬停出底色）。
    pub fn ghost_button(&mut self, id: impl Into<String>, label: impl Into<String>) {
        self.button_kind(id.into(), label.into(), true);
    }

    fn button_kind(&mut self, id: String, label: String, ghost: bool) {
        let theme = self.ctx().theme.clone();
        let font = FontSpec { size: theme.sizes.ui, weight: Some(TextWeight::SemiBold), family: None };
        let for_width = label.clone();
        self.push(Item {
            width: Width::Fit(Box::new(move |metrics| metrics.width(&for_width, font) + BUTTON_PAD_X * 2.0)),
            fill_height: false,
            height: Box::new(move |_, _| BUTTON_H),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let lit = ctx.hovered(&id) || ctx.pressed(&id);
                let background = if ghost {
                    lit.then(|| theme::token(ColorToken::Hover))
                } else if lit {
                    Some(theme::token(ColorToken::Hover))
                } else {
                    Some(theme::token(ColorToken::Surface))
                };
                if let Some(fill) = background {
                    ctx.draw(DrawCommand::Rect {
                        x: visible.x,
                        y: visible.y,
                        w: visible.w,
                        h: visible.h,
                        fill: Some(fill),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(theme.radii.sm),
                    });
                }
                let text_color = theme::token(if ghost && !lit { ColorToken::TextSecondary } else { ColorToken::Text });
                let shown = text::elide(&mut ctx.metrics, &label, font, (rect.w - BUTTON_PAD_X * 2.0).max(0.0));
                let width = ctx.metrics.width(&shown, font);
                let height = ctx.metrics.measure(&shown, font, None).height;
                text::paint(
                    ctx,
                    &shown,
                    font,
                    text_color,
                    rect.x + (rect.w - width) / 2.0,
                    rect.y + (rect.h - height) / 2.0,
                    None,
                    None,
                    visible,
                );
                ctx.hit(&id, visible, HitCursor::Pointer);
                ctx.record(&id, Layout::Button);
            }),
        });
    }

    /// 单行输入框（撑满父宽）。
    pub fn text_input(&mut self, id: impl Into<String>, value: impl Into<String>, placeholder: impl Into<String>) {
        self.input_kind(id.into(), value.into(), placeholder.into(), None);
    }

    /// 单行输入框（固定宽）。
    pub fn text_input_sized(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        width: f32,
    ) {
        self.input_kind(id.into(), value.into(), placeholder.into(), Some(width));
    }

    fn input_kind(&mut self, id: String, value: String, placeholder: String, width: Option<f32>) {
        let theme = self.ctx().theme.clone();
        // 与宿主表单输入一致：值文字取 caption 档
        let font = FontSpec { size: theme.sizes.caption, weight: None, family: None };
        let for_layout = value.clone();
        self.push(Item {
            width: width.map(Width::Fixed).unwrap_or(Width::Fill),
            fill_height: false,
            height: Box::new(|_, _| INPUT_H),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let focused = ctx.focused(&id);
                draw_field(ctx, &theme, visible, focused);
                let inner = Rect::new(rect.x + PAD_X, rect.y, (rect.w - PAD_X * 2.0).max(1.0), rect.h);
                draw_input_text(ctx, &id, &value, &placeholder, font, inner, rect, focused, &theme, visible);
                ctx.hit(&id, visible, HitCursor::Text);
                ctx.record(&id, Layout::Input { inner, font, value: for_layout.clone() });
                ctx.focusables.push(id.clone());
            }),
        });
    }

    /// 下拉：展开时就地撑开后续内容（指令集没有覆盖绘制，浮层会被下层文字穿透）。
    pub fn select(&mut self, id: impl Into<String>, options: Vec<String>, selected: usize) {
        let id = id.into();
        let theme = self.ctx().theme.clone();
        let font = FontSpec { size: theme.sizes.caption, weight: None, family: None };
        let open = self.ctx().state.selects.get(&id).is_some_and(|state| state.open);
        let options = Rc::new(options);
        let count = options.len();
        let menu_height = if open { OPTION_GAP + OPTION_H * count as f32 } else { 0.0 };
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |_, _| INPUT_H + menu_height),
            draw: Box::new({
                let options = Rc::clone(&options);
                move |ctx, rect, clip| {
                    let visible = rect.intersect(clip);
                    if visible.is_empty() {
                        return;
                    }
                    let focused = ctx.focused(&id);
                    let trigger = Rect::new(rect.x, rect.y, rect.w, INPUT_H);
                    let trigger_visible = trigger.intersect(clip);
                    draw_field(ctx, &theme, trigger_visible, focused);
                    let current = options.get(selected).map(String::as_str).unwrap_or("");
                    let label = text::elide(&mut ctx.metrics, current, font, (trigger.w - PAD_X * 2.0 - 16.0).max(0.0));
                    let height = ctx.metrics.measure(&label, font, None).height;
                    text::paint(
                        ctx,
                        &label,
                        font,
                        theme::token(ColorToken::Text),
                        trigger.x + PAD_X,
                        trigger.y + (INPUT_H - height) / 2.0,
                        None,
                        None,
                        trigger_visible,
                    );
                    draw_chevron(ctx, trigger, trigger_visible);
                    ctx.hit(&id, trigger_visible, HitCursor::Pointer);
                    ctx.record(&id, Layout::Select { options: count });
                    ctx.focusables.push(id.clone());
                    if open {
                        let menu = Rect::new(rect.x, rect.y + INPUT_H + OPTION_GAP, rect.w, OPTION_H * count as f32);
                        draw_menu(ctx, &theme, &id, &options, selected, menu, clip);
                    }
                }
            }),
        });
    }

    /// 复选框（方框 + 标签，整行可点）。
    pub fn checkbox(&mut self, id: impl Into<String>, checked: bool, label: impl Into<String>) {
        let id = id.into();
        let label = label.into();
        let theme = self.ctx().theme.clone();
        let font = FontSpec { size: theme.sizes.ui, weight: None, family: None };
        let for_width = label.clone();
        let for_height = label.clone();
        self.push(Item {
            width: Width::Fit(Box::new(move |metrics| CHECK_BOX + 8.0 + metrics.width(&for_width, font))),
            fill_height: false,
            height: Box::new(move |metrics, _| CHECK_BOX.max(metrics.measure(&for_height, font, None).height) + 6.0),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                if ctx.hovered(&id) || ctx.pressed(&id) {
                    ctx.draw(DrawCommand::Rect {
                        x: visible.x,
                        y: visible.y,
                        w: visible.w,
                        h: visible.h,
                        fill: Some(theme::token(ColorToken::Hover)),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(theme.radii.xs),
                    });
                }
                let box_rect = Rect::new(rect.x + 2.0, rect.y + (rect.h - CHECK_BOX) / 2.0, CHECK_BOX, CHECK_BOX);
                let box_visible = box_rect.intersect(visible);
                if box_visible.is_empty() {
                    return;
                }
                ctx.draw(DrawCommand::Rect {
                    x: box_visible.x,
                    y: box_visible.y,
                    w: box_visible.w,
                    h: box_visible.h,
                    fill: None,
                    stroke: Some(theme::token(if checked { ColorToken::Accent } else { ColorToken::TextMuted })),
                    stroke_width: Some(1.0),
                    corner_radius: Some(theme.radii.xs),
                });
                if checked && box_rect == box_visible {
                    let glyph = [
                        CanvasPoint { x: box_rect.x + 3.0, y: box_rect.y + 7.5 },
                        CanvasPoint { x: box_rect.x + 6.0, y: box_rect.y + 10.5 },
                        CanvasPoint { x: box_rect.x + 11.5, y: box_rect.y + 4.0 },
                    ];
                    ctx.draw(DrawCommand::Path {
                        points: glyph.to_vec(),
                        closed: false,
                        fill: None,
                        stroke: Some(theme::token(ColorToken::Accent)),
                        stroke_width: Some(1.6),
                    });
                }
                let text_x = box_rect.right() + 8.0;
                let shown = text::elide(&mut ctx.metrics, &label, font, (rect.right() - text_x).max(0.0));
                let height = ctx.metrics.measure(&shown, font, None).height;
                text::paint(
                    ctx,
                    &shown,
                    font,
                    theme::token(ColorToken::Text),
                    text_x,
                    rect.y + (rect.h - height) / 2.0,
                    None,
                    None,
                    visible,
                );
                ctx.hit(&id, visible, HitCursor::Pointer);
                ctx.record(&id, Layout::Checkbox);
                ctx.focusables.push(id.clone());
            }),
        });
    }

    /// 滚动容器：高度吃满父容器剩余空间；内容按块滚动，滚出视口的块整块不画。
    pub fn scroll(&mut self, id: impl Into<String>, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.scroll_like(id.into(), gap, build);
    }

    /// 行列表：与 [`Ui::scroll`] 同构（子项按行/块边界对齐滚动）。
    pub fn list(&mut self, id: impl Into<String>, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.scroll_like(id.into(), gap, build);
    }

    fn scroll_like(&mut self, id: String, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height: true,
            height: Box::new(move |metrics, width| column_height(&for_size, metrics, width, gap)),
            draw: Box::new(move |ctx, rect, clip| draw_scroll(ctx, &shared, &id, rect, clip, gap)),
        });
    }
}

fn text_item(content: String, spec: FontSpec, color: ColorToken, wrap: bool) -> Item {
    let for_wrap_height = content.clone();
    let for_line_height = content.clone();
    let for_draw = content;
    Item {
        width: Width::Fill,
        fill_height: false,
        height: if wrap {
            Box::new(move |metrics, width| metrics.measure(&for_wrap_height, spec, Some(width)).height)
        } else {
            Box::new(move |metrics, _| metrics.measure(&for_line_height, spec, None).height)
        },
        draw: Box::new(move |ctx, rect, clip| {
            let visible = rect.intersect(clip);
            if visible.is_empty() {
                return;
            }
            if wrap {
                text::paint(
                    ctx,
                    &for_draw,
                    spec,
                    theme::token(color),
                    rect.x,
                    rect.y,
                    Some(rect.w),
                    Some(TextAlign::Start),
                    visible,
                );
            } else {
                let shown = text::elide(&mut ctx.metrics, &for_draw, spec, rect.w);
                text::paint(ctx, &shown, spec, theme::token(color), rect.x, rect.y, None, None, visible);
            }
        }),
    }
}

/// 表单控件底：主题表面底 + 常规边框，聚焦换强调边框；按可见区收敛（指令集没有裁剪）。
fn draw_field(ctx: &mut BuildCtx<'_>, theme: &Theme, visible: Rect, focused: bool) {
    ctx.draw(DrawCommand::Rect {
        x: visible.x,
        y: visible.y,
        w: visible.w,
        h: visible.h,
        fill: Some(theme::token(ColorToken::Surface)),
        stroke: Some(theme::token(if focused { ColorToken::Accent } else { ColorToken::BorderStrong })),
        stroke_width: Some(1.0),
        corner_radius: Some(theme.radii.sm),
    });
}

fn draw_chevron(ctx: &mut BuildCtx<'_>, trigger: Rect, visible: Rect) {
    if visible != trigger {
        return;
    }
    let cx = trigger.right() - PAD_X - 4.0;
    let cy = trigger.y + trigger.h / 2.0;
    let points = vec![
        CanvasPoint { x: cx - 4.0, y: cy - 2.0 },
        CanvasPoint { x: cx, y: cy + 2.0 },
        CanvasPoint { x: cx + 4.0, y: cy - 2.0 },
    ];
    ctx.draw(DrawCommand::Path {
        points,
        closed: false,
        fill: None,
        stroke: Some(theme::token(ColorToken::TextMuted)),
        stroke_width: Some(1.0),
    });
}

fn draw_menu(
    ctx: &mut BuildCtx<'_>,
    theme: &Theme,
    id: &str,
    options: &[String],
    selected: usize,
    menu: Rect,
    clip: Rect,
) {
    let visible = menu.intersect(clip);
    if visible.is_empty() {
        return;
    }
    ctx.draw(DrawCommand::Rect {
        x: visible.x,
        y: visible.y,
        w: visible.w,
        h: visible.h,
        fill: Some(theme::token(ColorToken::Surface)),
        stroke: Some(theme::token(ColorToken::Border)),
        stroke_width: Some(1.0),
        corner_radius: Some(theme.radii.sm),
    });
    let hover = ctx.state.selects.get(id).and_then(|state| state.hover);
    let font = FontSpec { size: theme.sizes.caption, weight: None, family: None };
    for (index, option) in options.iter().enumerate() {
        let row = Rect::new(menu.x + 2.0, menu.y + OPTION_H * index as f32 + 2.0, (menu.w - 4.0).max(0.0), OPTION_H - 2.0);
        let row_visible = row.intersect(clip);
        if row_visible.is_empty() {
            continue;
        }
        let background = if index == selected {
            Some(theme::token(ColorToken::AccentSoft))
        } else if hover == Some(index) {
            Some(theme::token(ColorToken::Hover))
        } else {
            None
        };
        if let Some(fill) = background {
            ctx.draw(DrawCommand::Rect {
                x: row_visible.x,
                y: row_visible.y,
                w: row_visible.w,
                h: row_visible.h,
                fill: Some(fill),
                stroke: None,
                stroke_width: None,
                corner_radius: Some(theme.radii.xs),
            });
        }
        let shown = text::elide(&mut ctx.metrics, option, font, (row.w - PAD_X * 2.0).max(0.0));
        let height = ctx.metrics.measure(&shown, font, None).height;
        text::paint(
            ctx,
            &shown,
            font,
            theme::token(ColorToken::Text),
            row.x + PAD_X,
            row.y + (row.h - height) / 2.0,
            None,
            None,
            row_visible,
        );
        let option_id = option_hit_id(id, index);
        ctx.hit(&option_id, row_visible, HitCursor::Pointer);
        ctx.record(&option_id, Layout::Option { select: id.to_string(), index });
    }
}

/// 下拉选项的内部命中 id（插件看不到，也不与控件 id 冲突）。
pub(crate) fn option_hit_id(select: &str, index: usize) -> String {
    format!("{select}::option:{index}")
}

#[allow(clippy::too_many_arguments)]
fn draw_input_text(
    ctx: &mut BuildCtx<'_>,
    id: &str,
    value: &str,
    placeholder: &str,
    font: FontSpec,
    inner: Rect,
    field: Rect,
    focused: bool,
    theme: &Theme,
    clip: Rect,
) {
    let default_state = InputState::default();
    let state = ctx.state.inputs.get(id).unwrap_or(&default_state);
    let line_height = ctx.metrics.measure("", font, None).height;
    let baseline = field.y + (field.h - line_height) / 2.0;
    if value.is_empty() && state.preedit.is_empty() && !focused {
        text::paint(
            ctx,
            placeholder,
            font,
            theme::token(ColorToken::TextMuted),
            inner.x,
            baseline,
            None,
            None,
            clip,
        );
        return;
    }
    let caret = if focused { state.caret.min(value.len()) } else { 0 };
    let anchor = if focused { state.anchor.min(value.len()) } else { 0 };
    let window = text::window(&mut ctx.metrics, value, font, inner.w, caret, anchor);
    let whole = field.y >= clip.y && field.bottom() <= clip.bottom() + 0.5;
    if let Some((low, high)) = window.selection.filter(|_| focused && whole) {
        ctx.draw(DrawCommand::Rect {
            x: inner.x + low,
            y: baseline,
            w: (high - low).max(1.0),
            h: line_height,
            fill: Some(theme.selection()),
            stroke: None,
            stroke_width: None,
            corner_radius: None,
        });
    }
    let shown = if window.truncated {
        format!("{}…", &value[window.start..window.end])
    } else {
        value[window.start..window.end].to_string()
    };
    text::paint(ctx, &shown, font, theme::token(ColorToken::Text), inner.x, baseline, None, None, clip);
    if !state.preedit.is_empty() {
        text::paint(
            ctx,
            &state.preedit,
            font,
            theme::token(ColorToken::Accent),
            inner.x + window.caret,
            baseline,
            Some((inner.w - window.caret).max(1.0)),
            None,
            clip,
        );
    }
    if focused && whole {
        ctx.draw(DrawCommand::Rect {
            x: inner.x + window.caret,
            y: baseline,
            w: 1.0,
            h: line_height,
            fill: Some(theme::token(ColorToken::Accent)),
            stroke: None,
            stroke_width: None,
            corner_radius: None,
        });
        ctx.ime = Some(Rect::new(inner.x + window.caret, baseline, 1.0, line_height));
    }
}

fn draw_scroll(ctx: &mut BuildCtx<'_>, items: &[Item], id: &str, rect: Rect, clip: Rect, gap: f32) {
    let visible = rect.intersect(clip);
    if visible.is_empty() {
        return;
    }
    let widths: Vec<f32> = items.iter().map(|item| child_width(item, &mut ctx.metrics, rect.w)).collect();
    let heights: Vec<f32> = items
        .iter()
        .zip(&widths)
        .map(|(item, width)| child_height(item, &mut ctx.metrics, *width))
        .collect();
    let content: f32 = heights.iter().sum::<f32>() + gap * items.len().saturating_sub(1) as f32;
    // 滚动位置只落在块边界上：半块露出会串到视口外（指令集没有裁剪）
    let mut stops = vec![0.0_f32];
    let mut cursor = 0.0_f32;
    for height in &heights {
        cursor += *height;
        stops.push(cursor);
        cursor += gap;
    }
    if let Some(last) = stops.last_mut() {
        *last = content.max(cursor - gap);
    }
    let max_offset = (content - rect.h).max(0.0);
    let mut offset = ctx.scrolls.get(id).copied().unwrap_or(0.0).clamp(0.0, max_offset);
    offset = nearest(&stops, offset).clamp(0.0, max_offset);
    ctx.scrolls.insert(id.to_string(), offset);
    ctx.record(id, Layout::Scroll { viewport: visible, content, stops });
    let mut y = rect.y - offset;
    for (index, item) in items.iter().enumerate() {
        let slot = Rect::new(rect.x, y, widths[index], heights[index]);
        let slot_visible = slot.intersect(visible);
        if !slot_visible.is_empty() {
            (item.draw)(ctx, slot, slot_visible);
        }
        y += heights[index] + gap;
    }
    if content > rect.h {
        draw_scrollbar(ctx, rect, visible, content, offset, max_offset);
    }
}

fn nearest(stops: &[f32], value: f32) -> f32 {
    stops
        .iter()
        .copied()
        .fold(0.0_f32, |best, stop| if (stop - value).abs() < (best - value).abs() { stop } else { best })
}

fn draw_scrollbar(ctx: &mut BuildCtx<'_>, rect: Rect, visible: Rect, content: f32, offset: f32, max_offset: f32) {
    const TRACK: f32 = 4.0;
    let track = Rect::new(rect.right() - TRACK, rect.y, TRACK, rect.h);
    let track_visible = track.intersect(visible);
    if track_visible.is_empty() {
        return;
    }
    ctx.draw(DrawCommand::Rect {
        x: track_visible.x,
        y: track_visible.y,
        w: track_visible.w,
        h: track_visible.h,
        fill: Some(theme::token(ColorToken::Border)),
        stroke: None,
        stroke_width: None,
        corner_radius: Some(TRACK / 2.0),
    });
    let thumb_height = (rect.h * rect.h / content).max(12.0).min(rect.h);
    let travel = rect.h - thumb_height;
    let ratio = if max_offset > 0.0 { offset / max_offset } else { 0.0 };
    let thumb = Rect::new(track.x, rect.y + travel * ratio, track.w, thumb_height);
    let thumb_visible = thumb.intersect(visible);
    if thumb_visible.is_empty() {
        return;
    }
    ctx.draw(DrawCommand::Rect {
        x: thumb_visible.x,
        y: thumb_visible.y,
        w: thumb_visible.w,
        h: thumb_visible.h,
        fill: Some(theme::token(ColorToken::BorderStrong)),
        stroke: None,
        stroke_width: None,
        corner_radius: Some(TRACK / 2.0),
    });
}
