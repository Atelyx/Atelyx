//! SDK 控件：文字、按钮、输入框、下拉、勾选与滚动容器。
//! 每个控件在自己的绘制闭包里写指令、命中区与布局记录；外观取主题 token，尺寸与宿主表单控件同档。

use std::rc::Rc;
use std::sync::Arc;

use atelyx_plugin_protocol::paint::{PaintPoint, DrawCommand, ImageFit, ImageSource, TextAlign};
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
    /// 表单值。
    Value,
}

impl TextRole {
    fn resolve(self, theme: &Theme) -> (FontSpec, ColorToken) {
        let sizes = &theme.sizes;
        match self {
            TextRole::Title => {
                (FontSpec { size: sizes.h2, weight: Some(TextWeight::SemiBold), family: None }, ColorToken::Text)
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
            TextRole::Value => (FontSpec { size: sizes.ui, weight: None, family: None }, ColorToken::Text),
        }
    }
}

/// 文字样式：角色定字号与字重，颜色 / 等宽 / 换行按需覆写。
#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    role: TextRole,
    color: Option<ColorToken>,
    mono: bool,
    wrap: bool,
}

impl TextStyle {
    pub fn new(role: TextRole) -> Self {
        Self { role, color: None, mono: false, wrap: false }
    }

    pub fn color(mut self, color: ColorToken) -> Self {
        self.color = Some(color);
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
                        });
                    }
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
        let for_size = Rc::clone(&shared);
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
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
        self.input_kind(id.into(), value.into(), placeholder.into(), None, false);
    }

    /// 密文输入框：值按掩码字符绘制，编辑语义与明文框一致。
    pub fn text_input_password(&mut self, id: impl Into<String>, value: impl Into<String>, placeholder: impl Into<String>) {
        self.input_kind(id.into(), value.into(), placeholder.into(), None, true);
    }

    /// 单行输入框（固定宽）。
    pub fn text_input_sized(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        width: f32,
    ) {
        self.input_kind(id.into(), value.into(), placeholder.into(), Some(width), false);
    }

    fn input_kind(&mut self, id: String, value: String, placeholder: String, width: Option<f32>, masked: bool) {
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
                draw_input_text(ctx, &id, &value, &placeholder, font, inner, rect, focused, &theme, visible, masked);
                ctx.hit(&id, visible, HitCursor::Text);
                ctx.record(&id, Layout::Input { inner, font, value: for_layout.clone() });
                ctx.focusables.push(id.clone());
            }),
        });
    }

    /// 多行输入框（撑满父宽、固定高）：软换行与显式换行都排，回车插入换行，
    /// 光标超出内框时逐行纵向跟随。
    pub fn text_area(&mut self, id: impl Into<String>, value: impl Into<String>, placeholder: impl Into<String>, height: f32) {
        self.text_area_kind(id.into(), value.into(), placeholder.into(), height, false);
    }

    /// 多行输入框（回车提交、Shift+回车换行）：聊天输入这类「回车即发送」的用法。
    pub fn text_area_submit(
        &mut self,
        id: impl Into<String>,
        value: impl Into<String>,
        placeholder: impl Into<String>,
        height: f32,
    ) {
        self.text_area_kind(id.into(), value.into(), placeholder.into(), height, true);
    }

    fn text_area_kind(&mut self, id: String, value: String, placeholder: String, height: f32, enter_submits: bool) {
        let theme = self.ctx().theme.clone();
        let font = FontSpec { size: theme.sizes.caption, weight: None, family: None };
        let for_layout = value.clone();
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |_, _| height),
            draw: Box::new(move |ctx, rect, clip| {
                let visible = rect.intersect(clip);
                if visible.is_empty() {
                    return;
                }
                let focused = ctx.focused(&id);
                draw_field(ctx, &theme, visible, focused);
                let inner = Rect::new(
                    rect.x + PAD_X,
                    rect.y + AREA_PAD_Y,
                    (rect.w - PAD_X * 2.0).max(1.0),
                    (rect.h - AREA_PAD_Y * 2.0).max(1.0),
                );
                draw_area_text(ctx, &id, &value, &placeholder, font, inner, focused, &theme, visible);
                ctx.hit(&id, visible, HitCursor::Text);
                ctx.record(&id, Layout::TextArea { inner, font, value: for_layout.clone(), enter_submits });
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
        self.scroll_kind(id.into(), None, gap, false, build);
    }

    /// 行列表：与 [`Ui::scroll`] 同构（子项按行/块边界对齐滚动）。
    pub fn list(&mut self, id: impl Into<String>, gap: f32, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.scroll_kind(id.into(), None, gap, false, build);
    }

    /// 撑满父容器剩余高度的滚动区；`follow_end` 语义与 [`Ui::scroll_sized`] 一致——
    /// 上一帧停在终点就跟到新终点，用户往回滚即脱钩。
    pub fn scroll_follow(&mut self, id: impl Into<String>, gap: f32, follow_end: bool, build: impl FnOnce(&mut Ui<'_, 'a>)) {
        self.scroll_kind(id.into(), None, gap, follow_end, build);
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
        self.scroll_kind(id.into(), Some(height), gap, follow_end, build);
    }

    fn scroll_kind(
        &mut self,
        id: String,
        height: Option<f32>,
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
            fill_height: height.is_none(),
            height: Box::new(move |metrics, width| match height {
                Some(height) => height,
                None => column_height(&for_size, metrics, width, gap),
            }),
            draw: Box::new(move |ctx, rect, clip| draw_scroll(ctx, &shared, &id, rect, clip, gap, follow_end)),
        });
    }

    /// 只读 Markdown：块由宿主解析（`markdown.parse`），排版与绘制在这里；
    /// 首次出现时该帧为空，取到解析结果后自动重建。版面按内容缓存，滚动不重排。
    pub fn markdown(&mut self, content: impl Into<String>) {
        let content = content.into();
        let blocks = Rc::new(self.ctx().markdown_blocks(&content));
        let theme = self.ctx().theme.clone();
        let cache = Arc::clone(&self.ctx().state.markdown_layouts);
        let key = super::markdown::content_key(&content);
        let for_height = Rc::clone(&blocks);
        self.push(Item {
            width: Width::Fill,
            fill_height: false,
            height: Box::new(move |metrics, width| {
                super::markdown::height(&cache, metrics, &theme, key, &for_height, width)
            }),
            draw: Box::new(move |ctx, rect, clip| super::markdown::paint(ctx, key, &blocks, rect, clip)),
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
    let display;
    let (caret, anchor) = if masked {
        let chars = value.chars().count();
        display = "•".repeat(chars);
        let mask_width = "•".len();
        let to_masked = |byte: usize| {
            let byte = byte.min(value.len());
            value.char_indices().take_while(|(index, _)| *index < byte).count() * mask_width
        };
        let caret = if focused { to_masked(state.caret) } else { 0 };
        let anchor = if focused { to_masked(state.anchor) } else { 0 };
        (caret, anchor)
    } else {
        display = value.to_string();
        let caret = if focused { state.caret.min(value.len()) } else { 0 };
        let anchor = if focused { state.anchor.min(value.len()) } else { 0 };
        (caret, anchor)
    };
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
        });
    }
    let shown = if window.truncated {
        format!("{}…", &display[window.start..window.end])
    } else {
        display[window.start..window.end].to_string()
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

/// 多行输入的文字、选区与光标：逐可视行绘制，纵向平移量只由光标行决定（见 `text::top_line`）。
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
) {
    let default_state = InputState::default();
    let state = ctx.state.inputs.get(id).unwrap_or(&default_state);
    let line_height = ctx.metrics.measure("", font, None).height.max(1.0);
    if value.is_empty() && state.preedit.is_empty() && !focused {
        text::paint(
            ctx,
            placeholder,
            font,
            theme::token(ColorToken::TextMuted),
            inner.x,
            inner.y,
            Some(inner.w),
            Some(TextAlign::Start),
            clip,
        );
        return;
    }
    let lines = text::lines(&mut ctx.metrics, value, font, inner.w);
    let caret = text::snap(value, state.caret);
    let anchor = text::snap(value, state.anchor);
    let caret_index = text::line_of(&lines, caret);
    let top = if focused { text::top_line(caret_index, line_height, inner.h) } else { 0 };
    let shift = inner.y - top as f32 * line_height;
    let (from, to) = (caret.min(anchor), caret.max(anchor));
    for (index, line) in lines.iter().enumerate().skip(top) {
        let y = shift + index as f32 * line_height;
        if y >= clip.bottom() {
            break;
        }
        if y + line_height <= clip.y {
            continue;
        }
        let segment = &value[line.start..line.end];
        if focused && from != to && line.start < to && line.end > from {
            let low = from.max(line.start) - line.start;
            let high = to.min(line.end) - line.start;
            let x1 = inner.x + text::offset_in_line(&mut ctx.metrics, segment, font, low);
            let x2 = if to > line.end {
                inner.right()
            } else {
                inner.x + text::offset_in_line(&mut ctx.metrics, segment, font, high)
            };
            let band = Rect::new(x1, y, (x2 - x1).max(1.0), line_height);
            if band.intersect(clip) == band {
                ctx.draw(DrawCommand::Rect {
                    x: band.x,
                    y: band.y,
                    w: band.w,
                    h: band.h,
                    fill: Some(theme.selection()),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: None,
                });
            }
        }
        if !segment.is_empty() {
            text::paint(ctx, segment, font, theme::token(ColorToken::Text), inner.x, y, None, None, clip);
        }
    }
    let caret_line = lines[caret_index];
    let caret_x = inner.x + text::offset_in_line(&mut ctx.metrics, &value[caret_line.start..caret_line.end], font, caret - caret_line.start);
    let caret_y = shift + caret_index as f32 * line_height;
    if !state.preedit.is_empty() {
        text::paint(
            ctx,
            &state.preedit,
            font,
            theme::token(ColorToken::Accent),
            caret_x,
            caret_y,
            Some((inner.right() - caret_x).max(1.0)),
            None,
            clip,
        );
    }
    if focused && caret_y >= clip.y && caret_y + line_height <= clip.bottom() + 0.5 {
        ctx.draw(DrawCommand::Rect {
            x: caret_x,
            y: caret_y,
            w: 1.0,
            h: line_height,
            fill: Some(theme::token(ColorToken::Accent)),
            stroke: None,
            stroke_width: None,
            corner_radius: None,
        });
        ctx.ime = Some(Rect::new(caret_x, caret_y, 1.0, line_height));
    }
}

/// 滚动条宽度（与宿主内嵌滚动条同宽）与内容让位。
const SCROLLBAR_W: f32 = 8.0;

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
    // 先按整宽排一遍判断是否溢出；溢出时内容宽度让出滚动条（滚动条占布局空间，不浮在内容上）
    let wide = scroll_metrics(ctx, items, rect.w, gap);
    let overflow = wide.content > rect.h;
    let inner_w = if overflow { (rect.w - SCROLLBAR_W).max(1.0) } else { rect.w };
    let laid = if overflow { scroll_metrics(ctx, items, inner_w, gap) } else { wide };
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
    });
    ctx.hit(&bar_id, track_visible, HitCursor::Default);
    ctx.record(&bar_id, Layout::ScrollBar { scroll: id.to_string(), track, thumb: thumb_height, max_offset });
}

/// 滚动条的内部命中 id（插件看不到，也不与控件 id 冲突）。
pub(crate) fn scrollbar_id(scroll: &str) -> String {
    format!("{scroll}::bar")
}
