//! SDK 控件：文字、按钮、输入框、下拉、勾选与滚动容器。
//! 每个控件在自己的绘制闭包里写指令、命中区与布局记录；外观取主题 token，尺寸与宿主表单控件同档。

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use atelyx_plugin_protocol::paint::{
    DrawCommand, ImageFit, ImageSource, PaintPoint, PaintShadow, TextAlign,
};
use atelyx_plugin_protocol::frame::HitCursor;
use atelyx_plugin_protocol::paint::{ColorToken, TextFamily, TextWeight};

use super::build::{child_height, child_width, column_height, draw_column, BuildCtx, Canvas, Item, Ui, Width};
use super::geom::Rect;
use super::model::{FontSpec, InputState, Layout};
use super::text;
use super::theme::{self, Theme, BUTTON_H, BUTTON_PAD_X, CHECK_BOX, INPUT_H, OPTION_GAP, OPTION_H, PAD_X};

/// 徽标的水平 / 垂直内边距。
const CHIP_PAD_X: f32 = 6.0;
const CHIP_PAD_Y: f32 = 2.0;
/// 进度条高度。
const PROGRESS_H: f32 = 6.0;
/// 图标钮命中区边长与图标绘制边长。
const ICON_BUTTON: f32 = 26.0;
const ICON_SIZE: f32 = 16.0;
/// 卡片内边距与子项间距。
const CARD_PAD: f32 = 9.0;
const CARD_GAP: f32 = 5.0;
/// 提示块的内边距。
const NOTICE_PAD: f32 = 8.0;

/// 多行输入框的垂直内边距。
const AREA_PAD_Y: f32 = 6.0;

/// 滚动区高度：撑满父容器剩余 / 定高 / 随内容收缩到上限。
#[derive(Clone, Copy, Debug, PartialEq)]
enum ScrollHeight {
    Fill,
    Fixed(f32),
    Max(f32),
}

/// 文字角色（字号 / 字重 / 颜色一并由主题决定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRole {
    /// 面板主标题。
    Title,
    /// 区块标题。
    Heading,
    /// 正文说明。
    Label,
    /// 字段标签。
    FieldLabel,
    /// 小号说明（可换行）。
    Caption,
    /// 极小说明（时间戳、摘要行）。
    Micro,
    /// 正文（阅读字号）。
    Body,
    /// 表单值。
    Value,
}

impl TextRole {
    fn resolve(self, theme: &Theme) -> (FontSpec, ColorToken) {
        let sizes = &theme.sizes;
        match self {
            TextRole::Title => {
                (FontSpec { size: sizes.h1, weight: Some(TextWeight::SemiBold), family: None }, ColorToken::Text)
            }
            TextRole::Heading => {
                (FontSpec { size: sizes.body, weight: Some(TextWeight::Medium), family: None }, ColorToken::Text)
            }
            TextRole::Label => (FontSpec { size: sizes.ui, weight: None, family: None }, ColorToken::TextSecondary),
            TextRole::FieldLabel => (
                FontSpec { size: sizes.caption, weight: Some(TextWeight::Medium), family: None },
                ColorToken::TextSecondary,
            ),
            TextRole::Caption => (FontSpec { size: sizes.caption, weight: None, family: None }, ColorToken::TextMuted),
            TextRole::Micro => (FontSpec { size: sizes.micro, weight: None, family: None }, ColorToken::TextMuted),
            TextRole::Body => (FontSpec { size: sizes.body, weight: None, family: None }, ColorToken::Text),
            TextRole::Value => (FontSpec { size: sizes.ui, weight: None, family: None }, ColorToken::Text),
        }
    }
}

/// 文字样式：角色定字号与字重，颜色 / 字重 / 等宽 / 换行按需覆写。
#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    role: TextRole,
    color: Option<ColorToken>,
    weight: Option<TextWeight>,
    mono: bool,
    wrap: bool,
}

/// 多行输入框的高度策略：定高，或随内容在上下限之间长高（超出上限后框内滚动）。
#[derive(Clone, Copy, Debug)]
pub enum AreaHeight {
    Fixed(f32),
    /// `min` / `max` 指文字区高度（不含内边距）。
    Auto { min: f32, max: f32 },
}

/// 四边内边距。
#[derive(Clone, Copy, Debug, Default)]
pub struct Edges {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

impl Edges {
    pub fn all(value: f32) -> Self {
        Self { left: value, right: value, top: value, bottom: value }
    }

    pub fn symmetric(horizontal: f32, vertical: f32) -> Self {
        Self { left: horizontal, right: horizontal, top: vertical, bottom: vertical }
    }
}

/// 容器与内容区的交互形态：可按（悬停底色 + 点击命中）或只悬停（只登记提示命中）。
#[derive(Clone, Copy)]
enum RegionKind {
    Click { radius: f32, selected: bool, enabled: bool },
    Hover,
}

impl RegionKind {
    fn paint(&self, ctx: &mut BuildCtx<'_>, id: &str, rect: Rect, visible: Rect) {
        match *self {
            RegionKind::Click { radius, selected, enabled } => {
                let lit = enabled && (ctx.hovered(id) || ctx.pressed(id));
                if selected || lit {
                    ctx.draw(DrawCommand::Rect {
                        x: rect.x,
                        y: rect.y,
                        w: rect.w,
                        h: rect.h,
                        fill: Some(theme::token(if selected {
                            ColorToken::AccentSoft
                        } else {
                            ColorToken::Hover
                        })),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(radius),
                        shadow: None,
                    });
                }
                if enabled {
                    // 先登记容器命中：子项随后登记，按「后声明在上」压住容器
                    ctx.hit(id, visible, HitCursor::Pointer);
                    ctx.record(id, Layout::Button);
                }
            }
            RegionKind::Hover => ctx.hit(id, visible, HitCursor::Default),
        }
    }
}

/// 容器外观：底色 / 边框 / 圆角都取主题 token，可带投影（浮层压住下层内容）。
#[derive(Clone, Copy, Debug)]
pub struct BoxStyle {
    pub fill: Option<ColorToken>,
    pub border: Option<ColorToken>,
    pub border_width: f32,
    pub radius: f32,
    pub shadow: Option<PaintShadow>,
}

impl BoxStyle {
    pub fn new(fill: Option<ColorToken>, border: Option<ColorToken>, radius: f32) -> Self {
        Self { fill, border, border_width: 1.0, radius, shadow: None }
    }

    pub fn hairline(mut self) -> Self {
        self.border_width = 0.5;
        self
    }

    pub fn shadow(mut self, shadow: PaintShadow) -> Self {
        self.shadow = Some(shadow);
        self
    }
}

/// 容器宽度：撑满父宽或固定宽度。
#[derive(Clone, Copy, Debug)]
pub enum BoxWidth {
    Fill,
    Fixed(f32),
}

impl TextStyle {
    pub fn new(role: TextRole) -> Self {
        Self { role, color: None, weight: None, mono: false, wrap: false }
    }

    pub fn color(mut self, color: ColorToken) -> Self {
        self.color = Some(color);
        self
    }

    /// 覆写角色默认字重（空态标题这类需要更重的单行文字）。
    pub fn weight(mut self, weight: TextWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    /// 等宽族（日志、命令预览这类按列对齐的内容）。
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// 按可用宽度换行（缺省单行省略）。
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    fn resolve(self, theme: &Theme) -> (FontSpec, ColorToken) {
        let (mut spec, color) = self.role.resolve(theme);
        if let Some(weight) = self.weight {
            spec.weight = Some(weight);
        }
        if self.mono {
            spec.family = Some(TextFamily::Mono);
        }
        (spec, self.color.unwrap_or(color))
    }
}

impl<'c, 'a> Ui<'c, 'a> {
    /// 单行文字（占满父宽，超出按宽度省略）。
    pub fn text(&mut self, content: impl Into<String>, role: TextRole) {
        let content = content.into();
        let (spec, color) = role.resolve(self.ctx().theme);
        self.push(text_item(content, spec, color, false));
    }

    /// 按样式绘制的文字（颜色 / 等宽 / 换行）。
    pub fn text_styled(&mut self, content: impl Into<String>, style: TextStyle) {
        let content = content.into();
        let (spec, color) = style.resolve(self.ctx().theme);
        self.push(text_item(content, spec, color, style.wrap));
    }

    /// 按内容定宽的单行文字（行内标题 / 摘要这类需要紧跟在前一项后面的文字）。
    pub fn text_styled_fit(&mut self, content: impl Into<String>, style: TextStyle) {
        let content = content.into();
        let (spec, color) = style.resolve(self.ctx().theme);
        let for_width = content.clone();
        let for_height = content.clone();
        let for_draw = content;
        self.push(Item {
            width: Width::Fit(Box::new(move |metrics| metrics.width(&for_width, spec))),
            fill_height: false,
            height: Box::new(move |metrics, _| {
                metrics.measure(&for_height, spec, None).height
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let line_height = text::line_height(&mut ctx.metrics, spec);
                text::paint_line(
                    ctx,
                    &for_draw,
                    spec,
                    theme::token(color),
                    rect.x,
                    rect.y,
                    line_height,
                    visible,
                );
            }),
        });
    }

    /// 徽标：小号圆角底色块 + 语义色文字（计数、状态标签）。
    pub fn chip(&mut self, label: impl Into<String>, color: ColorToken) {
        let label = label.into();
        let theme = self.ctx().theme.clone();
        let font = FontSpec { size: theme.sizes.caption, weight: Some(TextWeight::Medium), family: None };
        let for_width = label.clone();
        let for_height = label.clone();
        self.push(Item {
            width: Width::Fit(Box::new(move |metrics| metrics.width(&for_width, font) + CHIP_PAD_X * 2.0)),
            fill_height: false,
            height: Box::new(move |metrics, _| metrics.measure(&for_height, font, None).height + CHIP_PAD_Y * 2.0),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
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
                    corner_radius: Some(theme.radii.xs),
                    shadow: None,
                });
                let shown = text::elide(&mut ctx.metrics, &label, font, (rect.w - CHIP_PAD_X * 2.0).max(0.0));
                let height = text::line_height(&mut ctx.metrics, font);
                text::paint_line(
                    ctx,
                    &shown,
                    font,
                    theme::token(color),
                    rect.x + CHIP_PAD_X,
                    rect.y + (rect.h - height) / 2.0,
                    height,
                    visible,
                );
            }),
        });
    }

    /// 进度条（`value` 取 0–1；轨道走边框色，已完成的段走强调色）。
    pub fn progress(&mut self, value: f32) {
        let value = value.clamp(0.0, 1.0);
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(|_, _| PROGRESS_H),
            draw: Box::new(move |ctx, rect, clip| {
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
                    corner_radius: Some(PROGRESS_H / 2.0),
                    shadow: None,
                });
                let filled = rect.w * value;
                if filled > 0.0 {
                    let done = Rect::new(rect.x, rect.y, filled, rect.h).intersect(clip);
                    if !done.is_empty() {
                        ctx.draw(DrawCommand::Rect {
                            x: done.x,
                            y: done.y,
                            w: done.w,
                            h: done.h,
                            fill: Some(theme::token(ColorToken::Accent)),
                            stroke: None,
                            stroke_width: None,
                            corner_radius: Some(PROGRESS_H / 2.0),
                            shadow: None,
                        });
                    }
                }
            }),
        });
    }

    /// 按 lucide 名称绘制的装饰图标（固定边长）；表外名称不画。
    pub fn icon_named(&mut self, name: &str, size: f32, color: ColorToken) {
        let name = name.to_string();
        self.push(Item {
            width: Width::Fixed(size),
            fill_height: false,
            height: Box::new(move |_, _| size),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                super::icons::draw_named(ctx, &name, rect, theme::token(color));
            }),
        });
    }

    /// 按 lucide 名称绘制的装饰图标（自带颜色：目录图标按用户设置着色这类场景）。
    pub fn icon_named_rgba(&mut self, name: &str, size: f32, color: crate::protocol::paint::PaintColor) {
        let name = name.to_string();
        self.push(Item {
            width: Width::Fixed(size),
            fill_height: false,
            height: Box::new(move |_, _| size),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                super::icons::draw_named(ctx, &name, rect, color);
            }),
        });
    }

    /// 按 lucide 名称绘制的图标钮（指定图标边长与命中盒边长）。
    pub fn icon_button_named(
        &mut self,
        id: impl Into<String>,
        name: &str,
        icon_px: f32,
        box_px: f32,
        color: ColorToken,
        enabled: bool,
    ) {
        let id = id.into();
        let name = name.to_string();
        let theme = self.ctx().theme.clone();
        self.push(Item {
            width: Width::Fixed(box_px),
            fill_height: false,
            height: Box::new(move |_, _| box_px),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                if enabled && (ctx.hovered(&id) || ctx.pressed(&id)) {
                    ctx.draw(DrawCommand::Rect {
                        x: visible.x,
                        y: visible.y,
                        w: visible.w,
                        h: visible.h,
                        fill: Some(theme::token(ColorToken::Hover)),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(theme.radii.xs),
                        shadow: None,
                    });
                }
                let icon = Rect::new(
                    rect.x + (rect.w - icon_px) / 2.0,
                    rect.y + (rect.h - icon_px) / 2.0,
                    icon_px,
                    icon_px,
                );
                let tint = if enabled { color } else { ColorToken::TextMuted };
                super::icons::draw_named(ctx, &name, icon, theme::token(tint));
                if enabled {
                    ctx.hit(&id, visible, HitCursor::Pointer);
                    ctx.record(&id, Layout::Button);
                }
            }),
        });
    }

    /// 实心图标钮（主操作 / 停止这类有底色的方钮）。
    pub fn icon_button_filled(
        &mut self,
        id: impl Into<String>,
        name: &str,
        icon_px: f32,
        box_px: f32,
        background: ColorToken,
        foreground: ColorToken,
        enabled: bool,
    ) {
        self.icon_button_filled_color(
            id,
            name,
            icon_px,
            box_px,
            background,
            theme::token(foreground),
            enabled,
        );
    }

    /// 同 [`Ui::icon_button_filled`]，图标色可用自带 RGBA（强调底上的深色字）。
    pub fn icon_button_filled_color(
        &mut self,
        id: impl Into<String>,
        name: &str,
        icon_px: f32,
        box_px: f32,
        background: ColorToken,
        foreground: atelyx_plugin_protocol::paint::PaintColor,
        enabled: bool,
    ) {
        let id = id.into();
        let name = name.to_string();
        let theme = self.ctx().theme.clone();
        self.push(Item {
            width: Width::Fixed(box_px),
            fill_height: false,
            height: Box::new(move |_, _| box_px),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                ctx.draw(DrawCommand::Rect {
                    x: visible.x,
                    y: visible.y,
                    w: visible.w,
                    h: visible.h,
                    fill: Some(theme::token(if enabled {
                        background
                    } else {
                        ColorToken::SurfaceMuted
                    })),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: Some(theme.radii.sm),
                    shadow: None,
                });
                let icon = Rect::new(
                    rect.x + (rect.w - icon_px) / 2.0,
                    rect.y + (rect.h - icon_px) / 2.0,
                    icon_px,
                    icon_px,
                );
                super::icons::draw_named(ctx, &name, icon, foreground);
                if enabled {
                    ctx.hit(&id, visible, HitCursor::Pointer);
                    ctx.record(&id, Layout::Button);
                }
            }),
        });
    }

    /// 图标钮：固定方形命中区，悬停出底色；图标形状取自 [`super::icons`]。
    pub fn icon_button(&mut self, id: impl Into<String>, icon: super::icons::Icon) {
        self.icon_kind(id.into(), icon, true);
    }

    /// 可禁用的图标钮：禁用时图标退为哑色且不承接点击。
    pub fn icon_button_enabled(&mut self, id: impl Into<String>, icon: super::icons::Icon, enabled: bool) {
        self.icon_kind(id.into(), icon, enabled);
    }

    /// 装饰图标：固定方形、不承接点击（空态这类插图位）。
    pub fn icon(&mut self, icon: super::icons::Icon) {
        self.push(Item {
            width: Width::Fixed(ICON_SIZE),
            fill_height: false,
            height: Box::new(|_, _| ICON_SIZE),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                super::icons::draw(ctx, icon, rect, theme::token(ColorToken::TextMuted));
            }),
        });
    }

    /// 图片贴片：撑满宽度、固定高（缺省拉伸填满）。
    pub fn image(&mut self, source: ImageSource, height: f32) {
        self.image_kind(source, height, None);
    }

    /// 图片贴片（指定填充方式）：`cover` 等比覆盖、`contain` 等比留边。
    pub fn image_fit(&mut self, source: ImageSource, height: f32, fit: ImageFit) {
        self.image_kind(source, height, Some(fit));
    }

    fn image_kind(&mut self, source: ImageSource, height: f32, fit: Option<ImageFit>) {
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |_, _| height),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                // 拉伸按可见区绘制（与裁剪等价）；等比方式换框会改变缩放比例，只能整片画
                let (x, y, w, h) = match fit {
                    None => (visible.x, visible.y, visible.w, visible.h),
                    Some(_) => (rect.x, rect.y, rect.w, rect.h),
                };
                ctx.draw(DrawCommand::Image { source: source.clone(), x, y, w, h, fit });
            }),
        });
    }

    fn icon_kind(&mut self, id: String, icon: super::icons::Icon, enabled: bool) {
        let theme = self.ctx().theme.clone();
        self.push(Item {
            width: Width::Fixed(ICON_BUTTON),
            fill_height: false,
            height: Box::new(|_, _| ICON_BUTTON),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                if enabled && (ctx.hovered(&id) || ctx.pressed(&id)) {
                    ctx.draw(DrawCommand::Rect {
                        x: visible.x,
                        y: visible.y,
                        w: visible.w,
                        h: visible.h,
                        fill: Some(theme::token(ColorToken::Hover)),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: Some(theme.radii.sm),
                        shadow: None,
                    });
                }
                let glyph = Rect::new(rect.x + (rect.w - ICON_SIZE) / 2.0, rect.y + (rect.h - ICON_SIZE) / 2.0, ICON_SIZE, ICON_SIZE);
                let color = if enabled { ColorToken::TextSecondary } else { ColorToken::TextMuted };
                super::icons::draw(ctx, icon, glyph, theme::token(color));
                if enabled {
                    ctx.hit(&id, visible, HitCursor::Pointer);
                    ctx.record(&id, Layout::Button);
                }
            }),
        });
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
                    shadow: None,
                });
            }),
        });
    }

    /// 转圈指示：每拍一转十等分（约一秒一圈），尺寸固定为正方形。
    /// 相位由控件层的帧拍推进，插件不必自己算角度，也不必自己声明帧拍频率。
    pub fn spinner(&mut self, size: f32, color: ColorToken) {
        let phase = self.ctx().state.clock as f32 * (std::f32::consts::TAU / 10.0);
        self.spinner_at(size, color, phase);
    }

    /// 指定相位的转圈（弧度）：插件有更强的时间源时用。
    pub fn spinner_at(&mut self, size: f32, color: ColorToken, phase: f32) {
        let stroke = (size / 14.0 * 1.5).max(1.0);
        self.ctx().wants_ticks = true;
        self.push(Item {
            width: Width::Fixed(size),
            fill_height: false,
            height: Box::new(move |_, _| size),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let radius = (size - stroke) / 2.0;
                let cx = rect.x + size / 2.0;
                let cy = rect.y + size / 2.0;
                let sweep = std::f32::consts::FRAC_PI_2 * 3.0;
                let points = (0..=24)
                    .map(|step| {
                        let angle = phase + sweep * step as f32 / 24.0;
                        PaintPoint { x: cx + radius * angle.cos(), y: cy + radius * angle.sin() }
                    })
                    .collect();
                ctx.draw(DrawCommand::Path {
                    points,
                    closed: false,
                    fill: None,
                    stroke: Some(theme::token(color)),
                    stroke_width: Some(stroke),
                });
            }),
        });
    }

    /// 竖向色条：占满行高（会话激活轨、状态条）。
    pub fn bar(&mut self, width: f32, color: ColorToken) {
        self.push(Item {
            width: Width::Fixed(width),
            fill_height: true,
            height: Box::new(|_, _| 0.0),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                ctx.draw(DrawCommand::Rect {
                    x: visible.x,
                    y: visible.y,
                    w: visible.w,
                    h: visible.h,
                    fill: Some(theme::token(color)),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: None,
                    shadow: None,
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

    /// 纵向弹簧：与同列其它撑满项均分父容器剩余高度（内容上下居中、把尾部推到列底）。
    pub fn fill_spacer(&mut self) {
        self.push(Item {
            width: Width::Fill,
            fill_height: true,
            height: Box::new(|_, _| 0.0),
            draw: Box::new(|_, _, _| {}),
        });
    }

    /// 面板级空态（图标 + 标题 + 说明）：在可用区域内居中。
    /// 排版只在 `frame_builders::empty_state` 里实现一次，这里只按主题取字号与行高。
    pub fn empty_state(&mut self, icon: &str, title: &str, desc: &str) {
        self.empty_state_block(icon, title, desc, None);
    }

    /// 带入口按钮的空态：按钮排在说明下，图标到按钮整块居中。
    pub fn empty_state_action(&mut self, icon: &str, title: &str, desc: &str, id: &str, label: &str) {
        self.empty_state_block(icon, title, desc, Some((id, label)));
    }

    fn empty_state_block(
        &mut self,
        icon: &str,
        title: &str,
        desc: &str,
        action: Option<(&str, &str)>,
    ) {
        let title_text = crate::frame_builders::EmptyStateText {
            size: self.role_size(TextRole::Title),
            line: self.text_height(TextRole::Title),
        };
        let desc_text = crate::frame_builders::EmptyStateText {
            size: self.role_size(TextRole::Caption),
            line: self.text_height(TextRole::Caption),
        };
        let block = crate::frame_builders::empty_state_height(title_text.line, desc_text.line);
        let icon = icon.to_string();
        let title = title.to_string();
        let desc = desc.to_string();
        self.fill_spacer();
        self.canvas(block, move |canvas| {
            let area = canvas.area();
            for command in crate::frame_builders::empty_state(
                (area.x, area.y, area.w, block),
                &icon,
                &title,
                &desc,
                title_text,
                desc_text,
            ) {
                canvas.draw(command);
            }
        });
        if let Some((id, label)) = action {
            self.spacer(crate::frame_builders::EMPTY_GAP);
            self.center_column(0.0, move |ui| ui.button(id, label));
        }
        self.fill_spacer();
    }

    /// 固定宽度的行内空白。
    pub fn spacer_width(&mut self, width: f32) {
        self.push(Item {
            width: Width::Fixed(width),
            fill_height: false,
            height: Box::new(|_, _| 0.0),
            draw: Box::new(|_, _, _| {}),
        });
    }

    /// 弹簧占位：吃掉行内剩余宽度，把其后的收缩子项推到行尾。
    pub fn grow(&mut self) {
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(|_, _| 0.0),
            draw: Box::new(|_, _, _| {}),
        });
    }

    /// 常规按钮（次级底）。
    pub fn button(&mut self, id: impl Into<String>, label: impl Into<String>) {
        self.button_kind(id.into(), label.into(), false, true);
    }

    /// 幽灵按钮（无底色，悬停出底色）。
    pub fn ghost_button(&mut self, id: impl Into<String>, label: impl Into<String>) {
        self.button_kind(id.into(), label.into(), true, true);
    }

    /// 可禁用的常规按钮：禁用时文字退为哑色且不承接点击。
    pub fn button_enabled(&mut self, id: impl Into<String>, label: impl Into<String>, enabled: bool) {
        self.button_kind(id.into(), label.into(), false, enabled);
    }

    /// 可禁用的幽灵按钮。
    pub fn ghost_button_enabled(&mut self, id: impl Into<String>, label: impl Into<String>, enabled: bool) {
        self.button_kind(id.into(), label.into(), true, enabled);
    }

    fn button_kind(&mut self, id: String, label: String, ghost: bool, enabled: bool) {
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
                let lit = enabled && (ctx.hovered(&id) || ctx.pressed(&id));
                let background = if !enabled {
                    None
                } else if ghost {
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
                        shadow: None,
                    });
                }
                let text_color = theme::token(if !enabled {
                    ColorToken::TextMuted
                } else if ghost && !lit {
                    ColorToken::TextSecondary
                } else {
                    ColorToken::Text
                });
                let shown = text::elide(&mut ctx.metrics, &label, font, (rect.w - BUTTON_PAD_X * 2.0).max(0.0));
                let width = text::advance(&mut ctx.metrics, &shown, font);
                let height = text::line_height(&mut ctx.metrics, font);
                text::paint_line(
                    ctx,
                    &shown,
                    font,
                    text_color,
                    rect.x + (rect.w - width) / 2.0,
                    rect.y + (rect.h - height) / 2.0,
                    height,
                    visible,
                );
                if enabled {
                    ctx.hit(&id, visible, HitCursor::Pointer);
                    ctx.record(&id, Layout::Button);
                }
            }),
        });
    }

    /// 可点卡片：整块承接点击（选中档强调底色与描边），子项按列排布。
    pub fn card(&mut self, id: impl Into<String>, selected: bool, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.card_with_shadow(id, selected, None, build);
    }

    /// 带投影的卡片：浮层里的卡片用它压住下层内容（投影先于填充绘制）。
    pub fn card_with_shadow(
        &mut self,
        id: impl Into<String>,
        selected: bool,
        shadow: Option<PaintShadow>,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        let id = id.into();
        let theme = self.ctx().theme.clone();
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |metrics, width| {
                column_height(&for_size, metrics, (width - CARD_PAD * 2.0).max(1.0), CARD_GAP) + CARD_PAD * 2.0
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let lit = ctx.hovered(&id) || ctx.pressed(&id);
                let fill = if selected { ColorToken::AccentSoft } else if lit { ColorToken::Hover } else { ColorToken::Surface };
                ctx.draw(DrawCommand::Rect {
                    x: visible.x,
                    y: visible.y,
                    w: visible.w,
                    h: visible.h,
                    fill: Some(theme::token(fill)),
                    stroke: Some(theme::token(if selected { ColorToken::Accent } else { ColorToken::Border })),
                    stroke_width: Some(1.0),
                    corner_radius: Some(theme.radii.sm),
                    shadow,
                });
                // 命中区先于子项登记：宿主按声明逆序取最上面的命中，子控件才不会被子项之后的卡片命中盖住
                ctx.hit(&id, visible, HitCursor::Pointer);
                ctx.record(&id, Layout::Button);
                let inner = Rect::new(
                    rect.x + CARD_PAD,
                    rect.y + CARD_PAD,
                    (rect.w - CARD_PAD * 2.0).max(1.0),
                    (rect.h - CARD_PAD * 2.0).max(1.0),
                );
                draw_column(ctx, &shared, inner, visible, CARD_GAP);
            }),
        });
    }

    /// 内边距容器：子项按列排布并整体内缩（需要内容留白但滚动条贴面边缘时用）。
    pub fn pad(&mut self, horizontal: f32, vertical: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
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
                column_height(&for_size, metrics, (width - horizontal * 2.0).max(1.0), 0.0) + vertical * 2.0
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let inner = Rect::new(
                    rect.x + horizontal,
                    rect.y + vertical,
                    (rect.w - horizontal * 2.0).max(1.0),
                    (rect.h - vertical * 2.0).max(1.0),
                );
                let visible = inner.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                draw_column(ctx, &shared, inner, visible, 0.0);
            }),
        });
    }

    /// 通用表面容器：底色 / 边框 / 圆角 / 四边内边距 / 子项间距都可指定。
    /// 宽度可撑满或固定；子项按列排布，不承接点击。
    pub fn boxed(
        &mut self,
        width: BoxWidth,
        style: BoxStyle,
        pad: Edges,
        gap: f32,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let fill_height = shared.iter().any(|item| item.fill_height);
        let for_size = Rc::clone(&shared);
        let item_width = match width {
            BoxWidth::Fill => Width::Fill,
            BoxWidth::Fixed(width) => Width::Fixed(width),
        };
        self.push(Item {
            width: item_width,
            fill_height,
            height: Box::new(move |metrics, width| {
                column_height(
                    &for_size,
                    metrics,
                    (width - pad.left - pad.right).max(1.0),
                    gap,
                ) + pad.top
                    + pad.bottom
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                if style.fill.is_some() || style.border.is_some() {
                    ctx.draw(DrawCommand::Rect {
                        x: visible.x,
                        y: visible.y,
                        w: visible.w,
                        h: visible.h,
                        fill: style.fill.map(theme::token),
                        stroke: style.border.map(theme::token),
                        stroke_width: style.border.map(|_| style.border_width),
                        corner_radius: Some(style.radius),
                        shadow: style.shadow,
                    });
                }
                let inner = Rect::new(
                    rect.x + pad.left,
                    rect.y + pad.top,
                    (rect.w - pad.left - pad.right).max(1.0),
                    (rect.h - pad.top - pad.bottom).max(1.0),
                );
                let visible = inner.intersect(clip);
                if !visible.is_empty() {
                    draw_column(ctx, &shared, inner, visible, gap);
                }
            }),
        });
    }

    /// 无边框行容器：整行可点，选中 / 悬停出底色（会话行、折叠块头、菜单行）。
    pub fn click_row(
        &mut self,
        id: impl Into<String>,
        pad: Edges,
        radius: f32,
        selected: bool,
        enabled: bool,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        self.click_box(
            id,
            BoxWidth::Fill,
            pad,
            radius,
            selected,
            enabled,
            build,
        );
    }

    /// 定宽 / 撑满的可点无边框容器（工具排触发钮这类按内容定宽的行）。
    pub fn click_box(
        &mut self,
        id: impl Into<String>,
        width: BoxWidth,
        pad: Edges,
        radius: f32,
        selected: bool,
        enabled: bool,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        self.region_box(
            id.into(),
            width,
            pad,
            RegionKind::Click { radius, selected, enabled },
            build,
        );
    }

    /// 只参与悬停的容器：不画悬停底色、不响应点击，用于「只呈现、不可操作」的行。
    /// 行上可挂 [`Ui::hint`]（悬停提示需要该行有命中区才能触发）。
    pub fn hover_row(&mut self, id: impl Into<String>, pad: Edges, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.hover_box(id, BoxWidth::Fill, pad, build);
    }

    /// 定宽 / 撑满的只悬停容器（形态同 [`Ui::hover_row`]）。
    pub fn hover_box(
        &mut self,
        id: impl Into<String>,
        width: BoxWidth,
        pad: Edges,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        self.region_box(id.into(), width, pad, RegionKind::Hover, build);
    }

    /// 可点行（定底色）：拖拽落点高亮、选中态背景这类需要自带底色的整行，
    /// 命中与悬停态仍走统一路径，点击产出 `Click`。
    pub fn click_row_tinted(
        &mut self,
        id: impl Into<String>,
        width: BoxWidth,
        pad: Edges,
        tint: Option<ColorToken>,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        let id = id.into();
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_size = Rc::clone(&shared);
        let item_width = match width {
            BoxWidth::Fill => Width::Fill,
            BoxWidth::Fixed(width) => Width::Fixed(width),
        };
        self.push(Item {
            width: item_width,
            fill_height: false,
            height: Box::new(move |metrics, width| {
                column_height(&for_size, metrics, (width - pad.left - pad.right).max(1.0), 0.0)
                    + pad.top
                    + pad.bottom
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                RegionKind::Click { radius: 0.0, selected: false, enabled: true }
                    .paint(ctx, &id, rect, visible);
                if let Some(color) = tint {
                    ctx.draw(crate::protocol::paint::DrawCommand::Rect {
                        x: rect.x,
                        y: rect.y,
                        w: rect.w,
                        h: rect.h,
                        fill: Some(theme::token(color)),
                        stroke: None,
                        stroke_width: None,
                        corner_radius: None,
                        shadow: None,
                    });
                }
                let inner = Rect::new(
                    rect.x + pad.left,
                    rect.y + pad.top,
                    (rect.w - pad.left - pad.right).max(1.0),
                    (rect.h - pad.top - pad.bottom).max(1.0),
                );
                let visible = inner.intersect(clip);
                if !visible.is_empty() {
                    draw_column(ctx, &shared, inner, visible, 0.0);
                }
            }),
        });
    }

    /// 可按 / 只悬停两种容器的共同排布：内容宽度、内边距与裁剪一致，差异只在
    /// 悬停底色的绘制与命中登记方式。
    fn region_box(
        &mut self,
        id: String,
        width: BoxWidth,
        pad: Edges,
        kind: RegionKind,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_size = Rc::clone(&shared);
        let item_width = match width {
            BoxWidth::Fill => Width::Fill,
            BoxWidth::Fixed(width) => Width::Fixed(width),
        };
        self.push(Item {
            width: item_width,
            fill_height: false,
            height: Box::new(move |metrics, width| {
                column_height(&for_size, metrics, (width - pad.left - pad.right).max(1.0), 0.0)
                    + pad.top
                    + pad.bottom
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                kind.paint(ctx, &id, rect, visible);
                let inner = Rect::new(
                    rect.x + pad.left,
                    rect.y + pad.top,
                    (rect.w - pad.left - pad.right).max(1.0),
                    (rect.h - pad.top - pad.bottom).max(1.0),
                );
                let visible = inner.intersect(clip);
                if !visible.is_empty() {
                    draw_column(ctx, &shared, inner, visible, 0.0);
                }
            }),
        });
    }

    /// 固定尺寸图片贴片。
    pub fn image_sized(&mut self, source: ImageSource, width: f32, height: f32, fit: Option<ImageFit>) {
        self.push(Item {
            width: Width::Fixed(width),
            fill_height: false,
            height: Box::new(move |_, _| height),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let (x, y, w, h) = match fit {
                    None => (visible.x, visible.y, visible.w, visible.h),
                    Some(_) => (rect.x, rect.y, rect.w, rect.h),
                };
                ctx.draw(DrawCommand::Image { source: source.clone(), x, y, w, h, fit });
            }),
        });
    }

    /// 透明命中区（撑满父容器剩余空间）；用于浮层背板「点外面关闭」。
    pub fn hit_area(&mut self, id: impl Into<String>) {
        let id = id.into();
        self.push(Item {
            width: Width::Fill,
            fill_height: true,
            height: Box::new(|_, _| 0.0),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                ctx.hit(&id, visible, HitCursor::Default);
                ctx.record(&id, Layout::Button);
            }),
        });
    }

    /// 量算单行文字宽高（当前主题字号与字宽）。
    pub fn text_width(&mut self, text: &str, role: TextRole) -> f32 {
        let (font, _) = role.resolve(self.ctx().theme);
        self.ctx.metrics.width(text, font)
    }

    /// 角色的字号（标题 / 正文 / 说明等档位）：自绘控件要按同档字号排布时用。
    pub fn role_size(&mut self, role: TextRole) -> f32 {
        let (font, _) = role.resolve(self.ctx().theme);
        font.size
    }

    /// 单行文字行高（按角色取字号与字重）：菜单行、候选行这类自估行高的布局用。
    pub fn text_height(&mut self, role: TextRole) -> f32 {
        let (font, _) = role.resolve(self.ctx().theme);
        text::line_height(&mut self.ctx().metrics, font)
    }

    /// 自适应多行输入框的实际高度（与 `text_area_submit_auto` 同口径）：
    /// 依赖输入区高度排布浮层 / 浮钮的调用方据此算位置。
    pub fn auto_area_height(
        &mut self,
        id: &str,
        value: &str,
        role: TextRole,
        width: f32,
        min: f32,
        max: f32,
        pad: Edges,
    ) -> f32 {
        let (font, _) = role.resolve(self.ctx().theme);
        let state = self.ctx().state.inputs.get(id);
        let caret = state.map_or(value.len(), |item| item.caret.min(value.len()));
        let preedit = state.map_or("", |item| item.preedit.as_str());
        let composed = text::compose(value, text::Composed::new(caret, preedit), preedit);
        auto_area_height(&mut self.ctx().metrics, &composed, font, width, min, max, pad)
    }

    /// 面板：圆角表面容器（承接子项，不承接点击）。
    pub fn panel(&mut self, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        let theme = self.ctx().theme.clone();
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |metrics, width| {
                column_height(&for_size, metrics, (width - CARD_PAD * 2.0).max(1.0), CARD_GAP) + CARD_PAD * 2.0
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
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
                    shadow: None,
                });
                let inner = Rect::new(
                    rect.x + CARD_PAD,
                    rect.y + CARD_PAD,
                    (rect.w - CARD_PAD * 2.0).max(1.0),
                    (rect.h - CARD_PAD * 2.0).max(1.0),
                );
                draw_column(ctx, &shared, inner, visible, CARD_GAP);
            }),
        });
    }

    /// 提示块：整行底色 + 语义色描边与文字（错误 / 警告 / 门控这类需要一眼看见的说明）。
    pub fn notice(&mut self, text: impl Into<String>, color: ColorToken) {
        let text = text.into();
        let theme = self.ctx().theme.clone();
        let font = FontSpec { size: theme.sizes.ui, weight: None, family: None };
        let for_height = text.clone();
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |metrics, width| {
                text::wrapped_height(metrics, &for_height, font, (width - NOTICE_PAD * 2.0).max(1.0))
                    + NOTICE_PAD * 2.0
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                ctx.draw(DrawCommand::Rect {
                    x: visible.x,
                    y: visible.y,
                    w: visible.w,
                    h: visible.h,
                    fill: Some(theme::token(ColorToken::Surface)),
                    stroke: Some(theme::token(color)),
                    stroke_width: Some(1.0),
                    corner_radius: Some(theme.radii.sm),
                    shadow: None,
                });
                let inner = Rect::new(rect.x + NOTICE_PAD, rect.y + NOTICE_PAD, (rect.w - NOTICE_PAD * 2.0).max(1.0), rect.h);
                let lines = text::wrap_lines(&mut ctx.metrics, &text, font, inner.w);
                let line_height = text::line_height(&mut ctx.metrics, font);
                for (index, line) in lines.into_iter().enumerate() {
                    text::paint_line(
                        ctx,
                        line,
                        font,
                        theme::token(color),
                        inner.x,
                        inner.y + index as f32 * line_height,
                        line_height,
                        visible,
                    );
                }
            }),
        });
    }

    /// 单行输入框（撑满父宽）。
    pub fn text_input(&mut self, id: impl Into<String>, value: impl Into<String>, placeholder: impl Into<String>) {
        self.input_kind(id.into(), value.into(), placeholder.into(), None, false, false);
    }

    /// 密文输入框：值按掩码字符绘制，编辑语义与明文框一致。
    pub fn text_input_password(&mut self, id: impl Into<String>, value: impl Into<String>, placeholder: impl Into<String>) {
        self.input_kind(id.into(), value.into(), placeholder.into(), None, true, false);
    }

    /// 单行输入框（固定宽）。
    pub fn text_input_sized(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        width: f32,
    ) {
        self.input_kind(id.into(), value.into(), placeholder.into(), Some(width), false, false);
    }

    /// 行内单行输入框（透明底，聚焦才出强调描边）：属性槽位这类就地编辑用。
    pub fn text_input_inline(&mut self, id: impl Into<String>, value: impl Into<String>, placeholder: impl Into<String>) {
        self.input_kind(id.into(), value.into(), placeholder.into(), None, false, true);
    }

    /// 固定宽的行内单行输入框。
    pub fn text_input_inline_sized(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        width: f32,
    ) {
        self.input_kind(id.into(), value.into(), placeholder.into(), Some(width), false, true);
    }

    fn input_kind(
        &mut self,
        id: String,
        value: String,
        placeholder: String,
        width: Option<f32>,
        masked: bool,
        inline: bool,
    ) {
        let theme = self.ctx().theme.clone();
        // 与宿主表单输入一致：值文字取 caption 档
        let font = FontSpec { size: theme.sizes.caption, weight: None, family: None };
        let for_layout = value.clone();
        self.push(Item {
            width: width.map(Width::Fixed).unwrap_or(Width::Fill),
            fill_height: false,
            height: Box::new(move |metrics, _| {
                if inline {
                    text::line_height(metrics, font) + 4.0
                } else {
                    INPUT_H
                }
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let focused = ctx.focused(&id);
                if !inline {
                    draw_field(ctx, &theme, visible, focused);
                } else if focused {
                    draw_inline_field(ctx, &theme, visible);
                }
                let inner = Rect::new(rect.x + PAD_X, rect.y, (rect.w - PAD_X * 2.0).max(1.0), rect.h);
                draw_input_text(ctx, &id, &value, &placeholder, font, inner, rect, focused, &theme, visible, masked);
                ctx.hit(&id, visible, HitCursor::Text);
                ctx.record(&id, Layout::Input { inner, font, value: for_layout.clone() });
                ctx.focusables.push(id.clone());
            }),
        });
    }

    /// 多行输入框（撑满父宽、定高）：软换行与显式换行都排，回车插入换行，
    /// 光标超出内框时纵向跟随。
    pub fn text_area(&mut self, id: impl Into<String>, value: impl Into<String>, placeholder: impl Into<String>, height: f32) {
        self.text_area_kind(
            id.into(),
            value.into(),
            placeholder.into(),
            AreaHeight::Fixed(height),
            false,
            None,
            false,
            Edges::symmetric(PAD_X, AREA_PAD_Y),
            Vec::new(),
        );
    }

    /// 多行输入框（回车提交、Shift+回车换行）：聊天输入这类「回车即发送」的用法。
    pub fn text_area_submit(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        height: f32,
    ) {
        self.text_area_kind(
            id.into(),
            value.into(),
            placeholder.into(),
            AreaHeight::Fixed(height),
            true,
            None,
            false,
            Edges::symmetric(PAD_X, AREA_PAD_Y),
            Vec::new(),
        );
    }

    /// 无底色多行输入框：外层容器自绘输入盒时用；`font_size` = 正文阅读字号。
    pub fn text_area_submit_plain(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        height: f32,
        font_size: f32,
        pad: Edges,
    ) {
        self.text_area_kind(
            id.into(),
            value.into(),
            placeholder.into(),
            AreaHeight::Fixed(height),
            true,
            Some(font_size),
            true,
            pad,
            Vec::new(),
        );
    }

    /// 自适应高度的无底色多行输入框（回车换行、不提交）：内容在 `min`–`max` 之间长高，
    /// 超出后在框内滚动。整篇正文编辑这类要交给外层滚动容器的场景用它。
    pub fn text_area_plain_auto(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        min: f32,
        max: f32,
        font_size: f32,
        pad: Edges,
    ) {
        self.text_area_kind(
            id.into(),
            value.into(),
            String::new(),
            AreaHeight::Auto { min, max },
            false,
            Some(font_size),
            true,
            pad,
            Vec::new(),
        );
    }

    /// 定高的无底色多行输入框（回车换行、不提交）：空行行这类只占一行、不画任何框的输入面。
    pub fn text_area_plain_fixed(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        height: f32,
        font_size: f32,
        pad: Edges,
    ) {
        self.text_area_kind(
            id.into(),
            value.into(),
            String::new(),
            AreaHeight::Fixed(height),
            false,
            Some(font_size),
            true,
            pad,
            Vec::new(),
        );
    }

    /// 自适应高度的无底色多行输入框（回车提交）：内容在 `min`–`max` 之间长高，超出后在框内滚动。
    /// 正文里被登记为标记的区间（如 `#` 引用标签）以强调浅底高亮，点进去整段选中、边界退格整段删除。
    #[allow(clippy::too_many_arguments)]
    pub fn text_area_submit_auto(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        min: f32,
        max: f32,
        font_size: f32,
        pad: Edges,
        marks: Vec<(usize, usize)>,
    ) {
        self.text_area_kind(
            id.into(),
            value.into(),
            placeholder.into(),
            AreaHeight::Auto { min, max },
            true,
            Some(font_size),
            true,
            pad,
            marks,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn text_area_kind(
        &mut self,
        id: String,
        value: String,
        placeholder: String,
        height: AreaHeight,
        enter_submits: bool,
        font_size: Option<f32>,
        bare: bool,
        pad: Edges,
        marks: Vec<(usize, usize)>,
    ) {
        let theme = self.ctx().theme.clone();
        let font = FontSpec { size: font_size.unwrap_or(theme.sizes.caption), weight: None, family: None };
        let for_layout = value.clone();
        // 组合串也计入高度：拼音还没上屏时输入盒要跟着长高
        let for_height = {
            let state = self.ctx().state.inputs.get(&id);
            let caret = state.map_or(value.len(), |item| item.caret.min(value.len()));
            let preedit = state.map_or("", |item| item.preedit.as_str());
            text::compose(&value, text::Composed::new(caret, preedit), preedit).into_owned()
        };
        let for_marks = marks.clone();
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |metrics, width| match height {
                AreaHeight::Fixed(height) => height,
                AreaHeight::Auto { min, max } => {
                    auto_area_height(metrics, &for_height, font, width, min, max, pad)
                }
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let focused = ctx.focused(&id);
                if !bare {
                    draw_field(ctx, &theme, visible, focused);
                }
                let inner = Rect::new(
                    rect.x + pad.left,
                    rect.y + pad.top,
                    (rect.w - pad.left - pad.right).max(1.0),
                    (rect.h - pad.top - pad.bottom).max(1.0),
                );
                draw_area_text(
                    ctx,
                    &id,
                    &value,
                    &placeholder,
                    font,
                    inner,
                    focused,
                    &theme,
                    visible,
                    &marks,
                );
                ctx.hit(&id, visible, HitCursor::Text);
                ctx.record(
                    &id,
                    Layout::TextArea {
                        inner,
                        font,
                        value: for_layout.clone(),
                        enter_submits,
                        marks: for_marks.clone(),
                    },
                );
                ctx.focusables.push(id.clone());
            }),
        });
    }

    /// 下拉：展开时就地撑开后续内容（不做覆盖式浮层，选项与触发钮连续排布）。
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
                    let height = text::line_height(&mut ctx.metrics, font);
                    text::paint_line(
                        ctx,
                        &label,
                        font,
                        theme::token(ColorToken::Text),
                        trigger.x + PAD_X,
                        trigger.y + (INPUT_H - height) / 2.0,
                        height,
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
                        shadow: None,
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
                    shadow: None,
                });
                if checked && box_rect == box_visible {
                    let glyph = [
                        PaintPoint { x: box_rect.x + 3.0, y: box_rect.y + 7.5 },
                        PaintPoint { x: box_rect.x + 6.0, y: box_rect.y + 10.5 },
                        PaintPoint { x: box_rect.x + 11.5, y: box_rect.y + 4.0 },
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
                let height = text::line_height(&mut ctx.metrics, font);
                text::paint_line(
                    ctx,
                    &shown,
                    font,
                    theme::token(ColorToken::Text),
                    text_x,
                    rect.y + (rect.h - height) / 2.0,
                    height,
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
        self.scroll_kind(id.into(), ScrollHeight::Fill, gap, false, build);
    }

    /// 行列表：与 [`Ui::scroll`] 同构（子项按行/块边界对齐滚动）。
    pub fn list(&mut self, id: impl Into<String>, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.scroll_kind(id.into(), ScrollHeight::Fill, gap, false, build);
    }

    /// 撑满父容器剩余高度的滚动区；`follow_end` 语义与 [`Ui::scroll_sized`] 一致——
    /// 上一帧停在终点就跟到新终点，用户往回滚即脱钩。
    pub fn scroll_follow(&mut self, id: impl Into<String>, gap: f32, follow_end: bool, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.scroll_kind(id.into(), ScrollHeight::Fill, gap, follow_end, build);
    }

    /// 定高滚动区：`follow_end` 时，上一帧停在终点就自动跟到新的终点（新内容到达即贴底），
    /// 用户往回滚即脱钩。
    pub fn scroll_sized(
        &mut self,
        id: impl Into<String>,
        height: f32,
        gap: f32,
        follow_end: bool,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        self.scroll_kind(id.into(), ScrollHeight::Fixed(height), gap, follow_end, build);
    }

    /// 限高滚动区：高度随内容收缩，超过 `max_height` 才出现滚动条（候选这类按条数收高的浮层）。
    pub fn scroll_max(
        &mut self,
        id: impl Into<String>,
        max_height: f32,
        gap: f32,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        self.scroll_kind(id.into(), ScrollHeight::Max(max_height), gap, false, build);
    }

    fn scroll_kind(
        &mut self,
        id: String,
        height: ScrollHeight,
        gap: f32,
        follow_end: bool,
        build: impl FnOnce(&mut Ui<'_, 'a>),
    ) {
        let clip = self.clip;
        let mut sub = Ui::new(&mut *self.ctx, clip);
        build(&mut sub);
        let shared = Rc::new(sub.into_items());
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height: height == ScrollHeight::Fill,
            height: Box::new(move |metrics, width| match height {
                ScrollHeight::Fill | ScrollHeight::Max(_) => {
                    let content = column_height(&for_size, metrics, width, gap);
                    match height {
                        ScrollHeight::Max(max) => content.min(max),
                        _ => content,
                    }
                }
                ScrollHeight::Fixed(height) => height,
            }),
            draw: Box::new(move |ctx, rect, clip| draw_scroll(ctx, &shared, &id, rect, clip, gap, follow_end)),
        });
    }

    /// 虚拟滚动列表（撑满父容器剩余高度）：只为视口附近构造条目，屏外条目用上一帧
    /// 实测高与 `estimate_height` 估算总高。`key` 必须稳定标识条目内容。
    /// `viewport_hint` 是视口高度的近似值；偏小由 overscan 补足。
    pub fn virtual_scroll(
        &mut self,
        id: impl Into<String>,
        len: usize,
        gap: f32,
        follow_end: bool,
        viewport_hint: f32,
        estimate_height: f32,
        key: impl Fn(usize) -> u64,
        build_item: impl Fn(&mut Ui<'_, 'a>, usize),
    ) {
        self.virtual_scroll_kind(
            id.into(),
            None,
            len,
            gap,
            follow_end,
            viewport_hint,
            estimate_height,
            key,
            build_item,
        );
    }

    /// 定高虚拟滚动列表：其余语义同 [`Ui::virtual_scroll`]。
    pub fn virtual_scroll_sized(
        &mut self,
        id: impl Into<String>,
        height: f32,
        len: usize,
        gap: f32,
        follow_end: bool,
        viewport_hint: f32,
        estimate_height: f32,
        key: impl Fn(usize) -> u64,
        build_item: impl Fn(&mut Ui<'_, 'a>, usize),
    ) {
        self.virtual_scroll_kind(
            id.into(),
            Some(height),
            len,
            gap,
            follow_end,
            viewport_hint,
            estimate_height,
            key,
            build_item,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn virtual_scroll_kind(
        &mut self,
        id: String,
        height: Option<f32>,
        len: usize,
        gap: f32,
        follow_end: bool,
        viewport_hint: f32,
        estimate_height: f32,
        key: impl Fn(usize) -> u64,
        build_item: impl Fn(&mut Ui<'_, 'a>, usize),
    ) {
        let clip = self.clip;
        let keys: Vec<u64> = (0..len).map(&key).collect();
        let cache = Arc::clone(&self.ctx.state.virtual_heights);
        let heights: Vec<f32> = {
            let mut heights = cache.lock().unwrap();
            let current: HashSet<u64> = keys.iter().copied().collect();
            heights.retain(|(list, key), _| list != &id || current.contains(key));
            keys.iter()
                .map(|key| heights.get(&(id.clone(), *key)).copied().unwrap_or(estimate_height))
                .collect()
        };
        let max_offset = (virtual_content(&heights, gap) - viewport_hint.max(1.0)).max(0.0);
        let previous = self.ctx.scrolls.get(&id).copied().unwrap_or(0.0);
        let previous_max = self.ctx.scroll_max.get(&id).copied().unwrap_or(0.0);
        let offset = virtual_offset(follow_end, previous, previous_max, max_offset);
        let plan = virtual_window(&heights, gap, offset, viewport_hint.max(1.0));
        let mut items = Vec::new();
        if plan.top > 0.0 {
            items.push(spacer_item(plan.top));
        }
        for index in plan.start..plan.end {
            let mut one = Ui::new(&mut *self.ctx, clip);
            build_item(&mut one, index);
            let children = Rc::new(one.into_items());
            let cache = Arc::clone(&cache);
            let list = id.clone();
            let item_key = keys[index];
            let for_height = Rc::clone(&children);
            items.push(Item {
                width: Width::Fill,
                fill_height: false,
                height: Box::new(move |metrics, width| {
                    let height = column_height(&for_height, metrics, width, 0.0);
                    cache.lock().unwrap().insert((list.clone(), item_key), height);
                    height
                }),
                draw: Box::new(move |ctx, rect, clip| {
                    let visible = rect.intersect(clip);
                    if !visible.is_empty() {
                        draw_column(ctx, &children, rect, visible, 0.0);
                    }
                }),
            });
        }
        if plan.end < len {
            items.push(spacer_item(plan.bottom.max(0.0)));
        }
        let shared = Rc::new(items);
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height: height.is_none(),
            height: Box::new(move |metrics, width| match height {
                Some(height) => height,
                None => column_height(&for_size, metrics, width, gap),
            }),
            draw: Box::new(move |ctx, rect, clip| draw_scroll(ctx, &shared, &id, rect, clip, gap, follow_end)),
        });
    }

    /// 给控件挂一条悬停提示：指针停够 400ms 由控件层画在指针下沿，插件不必自己管浮层。
    pub fn hint(&mut self, id: &str, text: &str) {
        self.ctx().declare_hint(id, text);
    }

    /// 只读 Markdown：解析（格式库）与排版都在控件层，版面按内容与宽度缓存，滚动不重排。
    /// 代码块头的「复制」由控件层自持（按下即写剪贴板），插件不必为它接线。
    pub fn markdown(&mut self, content: impl Into<String>) {
        self.markdown_with(content, Vec::new());
    }

    /// 带引用候选的只读 Markdown：`#笔记：方案.md` 这类标签按胶囊绘制（对话气泡用）。
    pub fn markdown_with(
        &mut self,
        content: impl Into<String>,
        mentions: Vec<atelyx_format::markdown::MentionCandidate>,
    ) {
        let content = content.into();
        let id = self.ctx().next_markdown_id(super::markdown::content_key(&content, &mentions));
        self.markdown_item(id, content, mentions, None);
    }

    /// 带调用方指定 id 的只读 Markdown：命中 / 选区 / 双击都按该 id 回发，面内排版后可
    /// 用 `FrameUi::rect` 取到块矩形做逐块行为（块级编辑这类按命中块分派）。
    pub fn markdown_named(
        &mut self,
        id: impl Into<String>,
        content: impl Into<String>,
        mentions: Vec<atelyx_format::markdown::MentionCandidate>,
    ) {
        self.markdown_item(id.into(), content.into(), mentions, None);
    }

    /// 单个列表项的只读 Markdown：`index` = 该项在原列表中的序号（0 起）。
    /// 单独渲染一项时按它画序号，避免有序列表每项都退化成 `1.`。
    pub fn markdown_list_item_named(
        &mut self,
        id: impl Into<String>,
        content: impl Into<String>,
        index: usize,
        mentions: Vec<atelyx_format::markdown::MentionCandidate>,
    ) {
        self.markdown_item(id.into(), content.into(), mentions, Some(index));
    }

    fn markdown_item(
        &mut self,
        id: String,
        content: String,
        mentions: Vec<atelyx_format::markdown::MentionCandidate>,
        list_start: Option<usize>,
    ) {
        let theme = self.ctx().theme.clone();
        let cache = Arc::clone(&self.ctx().state.markdown_layouts);
        let soft_wrap = self.ctx().soft_wrap;
        let key = super::markdown::content_key(&content, &mentions);
        // 同一段项源码在不同列表位置要各自排版：起始序号一并计入缓存键
        let key = match list_start {
            Some(start) => key ^ (start as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15),
            None => key,
        };
        // 折叠软换行改变版面：与保留换行的版面分开缓存
        let key = if soft_wrap { key } else { key ^ 0x5F35_6E4B_2A17_9C31 };
        let start = list_start.unwrap_or(0);
        let for_height = content.clone();
        let height_mentions = mentions.clone();
        let draw_mentions = mentions;
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |metrics, width| {
                super::markdown::height(
                    &cache,
                    metrics,
                    &theme,
                    key,
                    &for_height,
                    &height_mentions,
                    width,
                    start,
                    soft_wrap,
                )
            }),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                // 正文可拖选：命中声明为文本形态，按下 / 拖动由输入路由换算成行列。
                // 复制钮与任务框在 paint 里后声明，同帧内后声明者优先，子区点击不会被整块吞掉。
                ctx.hit(&id, visible, atelyx_plugin_protocol::frame::HitCursor::Text);
                ctx.record(&id, super::model::Layout::Markdown { rect });
                super::markdown::paint(ctx, &id, key, &content, &draw_mentions, rect, clip, start, soft_wrap);
            }),
        });
    }

    /// 自绘块：撑满宽度、固定高；闭包拿画布写指令并按子区登记命中。
    pub fn canvas(&mut self, height: f32, draw: impl Fn(&mut Canvas<'_, '_>) + 'static) {
        self.canvas_kind(Width::Fill, height, Box::new(draw));
    }

    /// 定宽自绘块（行内自绘小块）。
    pub fn canvas_sized(&mut self, width: f32, height: f32, draw: impl Fn(&mut Canvas<'_, '_>) + 'static) {
        self.canvas_kind(Width::Fixed(width), height, Box::new(draw));
    }

    fn canvas_kind(&mut self, width: Width, height: f32, draw: Box<dyn for<'s, 't> Fn(&mut Canvas<'s, 't>)>) {
        self.push(Item {
            width,
            fill_height: false,
            height: Box::new(move |_, _| height),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let mut canvas = Canvas::new(ctx, rect, visible);
                draw(&mut canvas);
            }),
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
            Box::new(move |metrics, width| text::wrapped_height(metrics, &for_wrap_height, spec, width))
        } else {
            Box::new(move |metrics, _| metrics.measure(&for_line_height, spec, None).height)
        },
        draw: Box::new(move |ctx, rect, clip| {
            let visible = rect.intersect(clip);
            if visible.is_empty() {
                return;
            }
            if wrap {
                // 插件自己切行：宿主按宽度量算整段会在改宽时为每一行登记一次量算
                let lines = text::wrap_lines(&mut ctx.metrics, &for_draw, spec, rect.w);
                let line_height = text::line_height(&mut ctx.metrics, spec);
                for (index, line) in lines.into_iter().enumerate() {
                    text::paint_line(
                        ctx,
                        line,
                        spec,
                        theme::token(color),
                        rect.x,
                        rect.y + index as f32 * line_height,
                        line_height,
                        visible,
                    );
                }
            } else {
                let shown = text::elide(&mut ctx.metrics, &for_draw, spec, rect.w);
                let line_height = text::line_height(&mut ctx.metrics, spec);
                text::paint_line(ctx, &shown, spec, theme::token(color), rect.x, rect.y, line_height, visible);
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
        shadow: None,
    });
}

/// 行内输入框底：不填底色，只给聚焦态一圈强调描边（就地编辑不改变行外观）。
fn draw_inline_field(ctx: &mut BuildCtx<'_>, theme: &Theme, visible: Rect) {
    ctx.draw(DrawCommand::Rect {
        x: visible.x,
        y: visible.y,
        w: visible.w,
        h: visible.h,
        fill: None,
        stroke: Some(theme::token(ColorToken::Accent)),
        stroke_width: Some(1.0),
        corner_radius: Some(theme.radii.xs),
        shadow: None,
    });
}

fn draw_chevron(ctx: &mut BuildCtx<'_>, trigger: Rect, visible: Rect) {
    if visible != trigger {
        return;
    }
    let cx = trigger.right() - PAD_X - 4.0;
    let cy = trigger.y + trigger.h / 2.0;
    let points = vec![
        PaintPoint { x: cx - 4.0, y: cy - 2.0 },
        PaintPoint { x: cx, y: cy + 2.0 },
        PaintPoint { x: cx + 4.0, y: cy - 2.0 },
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
        shadow: None,
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
                shadow: None,
            });
        }
        let shown = text::elide(&mut ctx.metrics, option, font, (row.w - PAD_X * 2.0).max(0.0));
        let height = text::line_height(&mut ctx.metrics, font);
        text::paint_line(
            ctx,
            &shown,
            font,
            theme::token(ColorToken::Text),
            row.x + PAD_X,
            row.y + (row.h - height) / 2.0,
            height,
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
    masked: bool,
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
    // 密文框只把值换成掩码字符（每颗一个字），光标 / 选区按字符数换算成掩码串里的字节位。
    // 插件换值时旧光标可能落在多字节字符内部，故按字符起点计数，不直接切字节。
    let display: String;
    let (caret, anchor);
    // 组合串在掩码框里不参与排版（掩码长度与串长不同源，混排会错位），只在光标处叠画
    let mut at = text::Composed::default();
    if masked {
        let chars = value.chars().count();
        display = "•".repeat(chars);
        let mask_width = "•".len();
        let to_masked = |byte: usize| {
            let byte = byte.min(value.len());
            value.char_indices().take_while(|(index, _)| *index < byte).count() * mask_width
        };
        caret = if focused { to_masked(state.caret) } else { 0 };
        anchor = if focused { to_masked(state.anchor) } else { 0 };
    } else {
        // 组合串作为光标处的临时文字进入排版：后面的字顺次右移
        let composed = text::Composed::new(text::snap(value, state.caret), &state.preedit);
        display = text::compose(value, composed, &state.preedit).into_owned();
        caret = if focused { composed.caret(text::snap(value, state.caret)) } else { 0 };
        anchor = if focused { composed.index(text::snap(value, state.anchor)) } else { 0 };
        at = composed;
    }
    let window = text::window(&mut ctx.metrics, &display, font, inner.w, caret, anchor);
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
            shadow: None,
        });
    }
    let shown = if window.truncated {
        format!("{}…", &display[window.start..window.end])
    } else {
        display[window.start..window.end].to_string()
    };
    text::paint(ctx, &shown, font, theme::token(ColorToken::Text), inner.x, baseline, None, None, clip);
    // 组合串换强调色：与正文同字形同位，重画窗口中相交的一段即完成上色
    let mark = if at.is_active() {
        overlap(at.span(), &text::Line { start: window.start, end: window.end })
    } else {
        None
    };
    if let Some((low, high)) = mark {
        let x = inner.x + text::offset_in_line(&mut ctx.metrics, &shown, font, low - window.start);
        text::paint_line(
            ctx,
            &display[low..high],
            font,
            theme::token(ColorToken::Accent),
            x,
            baseline,
            line_height,
            clip,
        );
    }
    if focused && whole {
        let lit = ctx.caret_lit();
        draw_caret_bar(ctx, inner.x + window.caret, baseline, line_height, clip, lit);
        // 候选窗贴组合串起点（串尾由本端自绘）
        let lead = at.index(text::snap(value, state.caret));
        let lead_x = if lead >= window.start {
            inner.x + text::offset_in_line(&mut ctx.metrics, &shown, font, lead - window.start)
        } else {
            inner.x
        };
        anchor_ime(ctx, lead_x, baseline, line_height, clip);
    }
}

/// 多行输入的文字、选区与光标：逐可视行绘制，纵向平移量只由光标行决定（见 `text::top_line`）。
/// 输入法组合串作为光标处的临时文字参与排版（后续字符被推开），以强调色绘制、光标停在串尾。
#[allow(clippy::too_many_arguments)]
fn draw_area_text(
    ctx: &mut BuildCtx<'_>,
    id: &str,
    value: &str,
    placeholder: &str,
    font: FontSpec,
    inner: Rect,
    focused: bool,
    theme: &Theme,
    clip: Rect,
    marks: &[(usize, usize)],
) {
    let default_state = InputState::default();
    let state = ctx.state.inputs.get(id).unwrap_or(&default_state);
    let line_height = ctx.metrics.measure("", font, None).height.max(1.0);
    let line_x = inner.x;
    // 空草稿恒显示占位符（取到焦点时也不消失），与原生输入框一致
    if value.is_empty() && state.preedit.is_empty() {
        text::paint(
            ctx,
            placeholder,
            font,
            theme::token(ColorToken::TextMuted),
            line_x,
            inner.y,
            Some(inner.w),
            Some(TextAlign::Start),
            clip,
        );
        if focused {
            let lit = ctx.caret_lit();
            draw_caret_bar(ctx, line_x, inner.y, line_height, clip, lit);
            anchor_ime(ctx, line_x, inner.y, line_height, clip);
        }
        return;
    }
    // 组合串作为光标处的临时文字进入排版：后面的字顺次右移、该换行就换行
    let at = text::Composed::new(text::snap(value, state.caret), &state.preedit);
    let composed = text::compose(value, at, &state.preedit);
    let caret = at.caret(text::snap(value, state.caret));
    let anchor = at.index(text::snap(value, state.anchor));
    let lines = text::lines(&mut ctx.metrics, &composed, font, inner.w);
    let caret_index = text::line_of(&lines, caret);
    let scroll = text::area_scroll(
        state.scroll,
        caret_index,
        line_height,
        inner.h,
        lines.len(),
        focused,
    );
    let shift = inner.y - scroll;
    let (from, to) = (caret.min(anchor), caret.max(anchor));
    for (index, line) in lines.iter().enumerate() {
        let y = shift + index as f32 * line_height;
        if y >= clip.bottom() {
            break;
        }
        if y + line_height <= clip.y {
            continue;
        }
        let segment = &composed[line.start..line.end];
        // 标记区间（`#` 引用标签这类整段实体）先画浅底，正文与选区压在其上
        for (mark_start, mark_end) in marks {
            let (mark_start, mark_end) = (*mark_start.min(mark_end), *mark_end.max(mark_start));
            let (start, end) = (at.index(mark_start), at.index(mark_end));
            if end <= line.start || start >= line.end {
                continue;
            }
            let low = start.max(line.start) - line.start;
            let high = end.min(line.end) - line.start;
            let x1 = line_x + text::offset_in_line(&mut ctx.metrics, segment, font, low);
            let x2 = line_x + text::offset_in_line(&mut ctx.metrics, segment, font, high);
            let band = Rect::new(
                x1,
                y + line_height * 0.15,
                (x2 - x1).max(1.0),
                line_height * 0.85,
            );
            if !band.covered_by(clip) {
                continue;
            }
            ctx.draw(DrawCommand::Rect {
                x: band.x,
                y: band.y,
                w: band.w,
                h: band.h,
                fill: Some(theme::token(ColorToken::AccentSoft)),
                stroke: None,
                stroke_width: None,
                corner_radius: Some(2.0),
                shadow: None,
            });
        }
        if focused && from != to && line.start < to && line.end > from {
            let low = from.max(line.start) - line.start;
            let high = to.min(line.end) - line.start;
            let x1 = line_x + text::offset_in_line(&mut ctx.metrics, segment, font, low);
            let x2 = if to > line.end {
                inner.right()
            } else {
                line_x + text::offset_in_line(&mut ctx.metrics, segment, font, high)
            };
            let band = Rect::new(x1, y, (x2 - x1).max(1.0), line_height);
            if band.covered_by(clip) {
                ctx.draw(DrawCommand::Rect {
                    x: band.x,
                    y: band.y,
                    w: band.w,
                    h: band.h,
                    fill: Some(theme.selection()),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: None,
                    shadow: None,
                });
            }
        }
        if !segment.is_empty() {
            text::paint(ctx, segment, font, theme::token(ColorToken::Text), line_x, y, None, None, clip);
        }
        // 组合串换强调色：与正文同字形同位，重画相交的一段即完成上色
        if at.is_active() {
            if let Some((low, high)) = overlap(at.span(), line) {
                let x = line_x + text::offset_in_line(&mut ctx.metrics, segment, font, low - line.start);
                text::paint_line(
                    ctx,
                    &composed[low..high],
                    font,
                    theme::token(ColorToken::Accent),
                    x,
                    y,
                    line_height,
                    clip,
                );
            }
        }
    }
    let caret_line = lines[caret_index];
    let caret_x = line_x
        + text::offset_in_line(
            &mut ctx.metrics,
            &composed[caret_line.start..caret_line.end],
            font,
            caret - caret_line.start,
        );
    let caret_y = shift + caret_index as f32 * line_height;
    if focused {
        let lit = ctx.caret_lit();
        draw_caret_bar(ctx, caret_x, caret_y, line_height, clip, lit);
        // 候选窗贴组合串起点（串尾由本端自绘）：串变长时候选窗不跟着右移
        let lead = at.index(text::snap(value, state.caret));
        let (ime_x, ime_y) = if lead < caret {
            let lead_index = text::line_of(&lines, lead);
            let line = lines[lead_index];
            let x = line_x
                + text::offset_in_line(
                    &mut ctx.metrics,
                    &composed[line.start..line.end],
                    font,
                    lead - line.start,
                );
            (x, shift + lead_index as f32 * line_height)
        } else {
            (caret_x, caret_y)
        };
        anchor_ime(ctx, ime_x, ime_y, line_height, clip);
    }
    // 内容超出可视高度：贴右缘的细条指示当前位置（浅色，不抢正文）
    let content = lines.len() as f32 * line_height;
    if content > inner.h + 0.5 {
        let track = (inner.h - 8.0).max(1.0);
        let thumb = (track * inner.h / content).max(24.0).min(track);
        let max = (content - inner.h).max(1.0);
        let top = inner.y + 4.0 + (scroll / max) * (track - thumb);
        ctx.draw(DrawCommand::Rect {
            x: inner.right() - 4.0,
            y: top,
            w: 3.0,
            h: thumb,
            fill: Some(theme::token(ColorToken::Border)),
            stroke: None,
            stroke_width: None,
            corner_radius: Some(1.5),
            shadow: None,
        });
    }
}

/// `span` 与一行文本相交的字节区间（无交集 = None）。
fn overlap(span: std::ops::Range<usize>, line: &text::Line) -> Option<(usize, usize)> {
    let low = span.start.max(line.start);
    let high = span.end.min(line.end);
    (low < high).then_some((low, high))
}

/// 自适应多行输入框的实际高度（含内边距）：文字区按整行取，
/// 视口高度与内容高度同为行高整数倍时滚动永远停在整行上，框边不会切出半截文字。
pub(crate) fn auto_area_height(
    metrics: &mut super::metrics::Metrics,
    value: &str,
    font: FontSpec,
    width: f32,
    min: f32,
    max: f32,
    pad: Edges,
) -> f32 {
    let line_height = text::line_height(metrics, font);
    let inner_w = (width - pad.left - pad.right).max(1.0);
    let content = text::wrapped_height(metrics, value, font, inner_w);
    let cap = (max / line_height).floor().max(1.0) * line_height;
    content.min(cap).max(min.min(cap)) + pad.top + pad.bottom
}

/// 光标竖条：按闪烁相位显隐；整行不在可见区内时不画（半截竖条会串到相邻区域）。
fn draw_caret_bar(ctx: &mut BuildCtx<'_>, x: f32, y: f32, line_height: f32, clip: Rect, lit: bool) {
    if !lit || y < clip.y || y + line_height > clip.bottom() + 0.5 {
        return;
    }
    ctx.draw(DrawCommand::Rect {
        x,
        y,
        w: 1.0,
        h: line_height,
        fill: Some(theme::token(ColorToken::Accent)),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    });
}

/// 输入法候选窗锚点：只在光标行可见时给出（宿主据它启用输入法并贴窗）。
fn anchor_ime(ctx: &mut BuildCtx<'_>, x: f32, y: f32, line_height: f32, clip: Rect) {
    if y < clip.y || y + line_height > clip.bottom() + 0.5 {
        return;
    }
    ctx.ime = Some(Rect::new(x, y, 1.0, line_height));
}

/// 滚动条宽度（与宿主内嵌滚动条同宽）与内容让位。
const SCROLLBAR_W: f32 = 8.0;

/// 虚拟列表视口外预构造的余量：滚动与改宽时新条目已在手上，不会先出空白。
const VIRTUAL_OVERSCAN: f32 = 360.0;

struct VirtualWindow {
    start: usize,
    end: usize,
    top: f32,
    bottom: f32,
}

fn spacer_item(height: f32) -> Item {
    Item {
        width: Width::Fill,
        fill_height: false,
        height: Box::new(move |_, _| height),
        draw: Box::new(|_, _, _| {}),
    }
}

fn virtual_content(heights: &[f32], gap: f32) -> f32 {
    heights.iter().sum::<f32>() + gap * heights.len().saturating_sub(1) as f32
}

/// 规划窗口用的偏移：只有上一帧停在终点才继续贴底，用户上翻后停在原偏移。
fn virtual_offset(follow_end: bool, previous: f32, previous_max: f32, max_offset: f32) -> f32 {
    if follow_end && previous >= previous_max - 0.5 {
        max_offset
    } else {
        previous.clamp(0.0, max_offset)
    }
}

/// 选出与 [offset - overscan, offset + 视口 + overscan] 相交的条目区间，
/// 并给出上下占位高度（保持总高与估算一致）。
fn virtual_window(heights: &[f32], gap: f32, offset: f32, viewport: f32) -> VirtualWindow {
    if heights.is_empty() {
        return VirtualWindow { start: 0, end: 0, top: 0.0, bottom: 0.0 };
    }
    let total = virtual_content(heights, gap);
    let top_edge = (offset - VIRTUAL_OVERSCAN).max(0.0);
    let bottom_edge = (offset + viewport + VIRTUAL_OVERSCAN).min(total);
    let mut position = 0.0;
    let mut start = heights.len();
    let mut end = heights.len();
    for (index, height) in heights.iter().enumerate() {
        let bottom = position + *height;
        if start == heights.len() && bottom >= top_edge {
            start = index;
        }
        if position >= bottom_edge {
            end = index;
            break;
        }
        position = bottom + gap;
    }
    if start == heights.len() {
        start = heights.len() - 1;
    }
    let start_position = heights[..start].iter().sum::<f32>() + gap * start as f32;
    let end_position = heights[..end].iter().sum::<f32>() + gap * end as f32;
    VirtualWindow {
        start,
        end,
        top: if start > 0 { (start_position - gap).max(0.0) } else { 0.0 },
        bottom: if end < heights.len() { (total - end_position).max(0.0) } else { 0.0 },
    }
}

fn draw_scroll(
    ctx: &mut BuildCtx<'_>,
    items: &[Item],
    id: &str,
    rect: Rect,
    clip: Rect,
    gap: f32,
    follow_end: bool,
) {
    let visible = rect.intersect(clip);
    if visible.is_empty() {
        return;
    }
    // 先按整宽判定是否溢出；溢出时内容宽度让出滚动条（滚动条占布局空间，不浮在内容上）
    let wide = scroll_metrics(ctx, items, rect.w, gap);
    let overflow = wide.content > rect.h;
    let laid = if overflow {
        scroll_metrics(ctx, items, (rect.w - SCROLLBAR_W).max(1.0), gap)
    } else {
        wide
    };
    let max_offset = (laid.content - rect.h).max(0.0);
    let previous = ctx.scrolls.get(id).copied().unwrap_or(0.0);
    let previous_max = ctx.scroll_max.get(id).copied().unwrap_or(0.0);
    let follow_before = ctx.follow_prev.get(id).copied().unwrap_or(false);
    ctx.follow_prev.insert(id.to_string(), follow_end);
    // 贴底跟随：刚打开跟随时跳到终点；此后上一帧停在终点就跟到新终点
    let offset = if follow_end && (!follow_before || previous >= previous_max - 0.5) {
        max_offset
    } else {
        previous.clamp(0.0, max_offset)
    };
    ctx.scrolls.insert(id.to_string(), offset);
    ctx.scroll_max.insert(id.to_string(), max_offset);
    ctx.record(id, Layout::Scroll { viewport: visible, content: laid.content });
    let mut y = rect.y - offset;
    for (index, item) in items.iter().enumerate() {
        let slot = Rect::new(rect.x, y, laid.widths[index], laid.heights[index]);
        let slot_visible = slot.intersect(visible);
        if !slot_visible.is_empty() {
            (item.draw)(ctx, slot, slot_visible);
        }
        y += laid.heights[index] + gap;
    }
    if overflow {
        let track = Rect::new(rect.right() - SCROLLBAR_W, rect.y, SCROLLBAR_W, rect.h);
        draw_scrollbar(ctx, id, track, visible, laid.content, offset, max_offset);
    }
}

struct ScrollMetrics {
    widths: Vec<f32>,
    heights: Vec<f32>,
    content: f32,
}

fn scroll_metrics(ctx: &mut BuildCtx<'_>, items: &[Item], width: f32, gap: f32) -> ScrollMetrics {
    let widths: Vec<f32> = items.iter().map(|item| child_width(item, &mut ctx.metrics, width)).collect();
    let heights: Vec<f32> = items
        .iter()
        .zip(&widths)
        .map(|(item, width)| child_height(item, &mut ctx.metrics, *width))
        .collect();
    let content: f32 = heights.iter().sum::<f32>() + gap * items.len().saturating_sub(1) as f32;
    ScrollMetrics { widths, heights, content }
}

/// 滚动条外观与宿主一致：下沉底轨道 + 三态滑块（常规 / 悬停 / 拖动强调色）。
fn draw_scrollbar(ctx: &mut BuildCtx<'_>, id: &str, track: Rect, visible: Rect, content: f32, offset: f32, max_offset: f32) {
    let track_visible = track.intersect(visible);
    if track_visible.is_empty() {
        return;
    }
    ctx.draw(DrawCommand::Rect {
        x: track_visible.x,
        y: track_visible.y,
        w: track_visible.w,
        h: track_visible.h,
        fill: Some(theme::token(ColorToken::SurfaceSunken)),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    });
    let thumb_height = (track.h * track.h / content).max(12.0).min(track.h);
    let travel = track.h - thumb_height;
    let ratio = if max_offset > 0.0 { offset / max_offset } else { 0.0 };
    let thumb = Rect::new(track.x, track.y + travel * ratio, track.w, thumb_height);
    let thumb_visible = thumb.intersect(visible);
    if thumb_visible.is_empty() {
        return;
    }
    let bar_id = scrollbar_id(id);
    let color = if ctx.state.drag_bar.as_deref() == Some(id) {
        ColorToken::Accent
    } else if ctx.hovered(&bar_id) {
        ColorToken::TextSecondary
    } else {
        ColorToken::BorderStrong
    };
    ctx.draw(DrawCommand::Rect {
        x: thumb_visible.x,
        y: thumb_visible.y,
        w: thumb_visible.w,
        h: thumb_visible.h,
        fill: Some(theme::token(color)),
        stroke: None,
        stroke_width: None,
        corner_radius: None,
        shadow: None,
    });
    ctx.hit(&bar_id, track_visible, HitCursor::Default);
    ctx.record(&bar_id, Layout::ScrollBar { scroll: id.to_string(), track, thumb: thumb_height, max_offset });
}

/// 滚动条的内部命中 id（插件看不到，也不与控件 id 冲突）。
pub(crate) fn scrollbar_id(scroll: &str) -> String {
    format!("{scroll}::bar")
}

#[cfg(test)]
mod virtual_tests {
    use super::{virtual_content, virtual_offset, virtual_window};

    #[test]
    fn window_covers_viewport_with_overscan_and_keeps_total_height() {
        let heights = vec![100.0; 200];
        let gap = 10.0;
        let total = virtual_content(&heights, gap);

        let top = virtual_window(&heights, gap, 0.0, 600.0);
        assert_eq!(top.start, 0);
        assert!(top.end > 0 && top.end < 30, "首批只构造视口附近条目：{}", top.end);
        assert_eq!(top.top, 0.0);
        assert_eq!(top.bottom, total - top.end as f32 * 110.0);

        let bottom = virtual_window(&heights, gap, total - 600.0, 600.0);
        assert_eq!(bottom.end, heights.len());
        assert!(bottom.start > 150, "贴底时跳过屏外头部：{}", bottom.start);
        assert_eq!(bottom.bottom, 0.0);
    }

    #[test]
    fn follow_end_does_not_pull_back_after_user_scrolls_up() {
        assert_eq!(virtual_offset(true, 0.0, 0.0, 900.0), 900.0, "首次贴底");
        assert_eq!(virtual_offset(true, 900.0, 900.0, 1200.0), 1200.0, "停在终点继续跟随");
        assert_eq!(virtual_offset(true, 300.0, 900.0, 1200.0), 300.0, "上翻后保持位置");
        assert_eq!(virtual_offset(false, 300.0, 900.0, 1200.0), 300.0);
    }

    #[test]
    fn tiny_list_lays_out_every_item() {
        let heights = vec![30.0, 40.0];
        let window = virtual_window(&heights, 5.0, 0.0, 600.0);
        assert_eq!((window.start, window.end), (0, 2));
        assert_eq!((window.top, window.bottom), (0.0, 0.0));
    }
}
