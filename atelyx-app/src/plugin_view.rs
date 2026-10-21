//! 插件面板渲染：把 [`ViewNode`] 树解释成 iced 控件，按节点声明绑定事件与交互外观。
//! 交互外观、尺寸上下限与点击命中由 [`NodeBox`] 统一收口：布局交给子项，只在子项矩形内绘制并约束尺寸。
//! 状态与接线在 [`crate::plugin_actions`]；本模块只读 `App` 里的投影缓存，不持运行时锁。

use iced::advanced::layout::{Layout, Limits, Node};
use iced::advanced::widget::tree;
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::{Horizontal, Vertical};
use iced::mouse;
use iced::widget::{
    column, container, grid, image, keyed_column, mouse_area, progress_bar, responsive, row, space,
    text_editor, text_input, Stack,
};
use iced::{Color, Element, Length, Padding, Rectangle, Renderer, Size, Theme, Vector};

use atelyx_core::plugin_runtime::{
    Align, Axis, BoxSize, ButtonVariant, CardTone, ColorSpec, ControlSize, EventKind, EventValue,
    ImageFit, ImageSource, Justify, NodeBody, PaddingSpec, RowHeight, SizeSpec, Surface, TextFamily,
    TextWeight, ViewEvent, ViewNode,
};

use crate::plugin_actions::{contribution, declares, image_key, input_display, node_value_text, PopupEntry};
use crate::ui::controls::{self, Variant};
use crate::ui::palette as pal;
use crate::ui::scroll;
use crate::ui::text;
use crate::{App, Message};

/// 容器缺省间距（插件未声明 gap 时）。
const DEFAULT_GAP: f32 = 6.0;
/// 单行输入的内边距（与设置表单同口径）。
const INPUT_PAD_X: f32 = 8.0;
const INPUT_PAD_Y: f32 = 6.0;
/// 多行输入高度。
const EDITOR_H: f32 = 96.0;
/// 画布缺省高度（未给 `height` 也未声明撑满时）。
const CANVAS_DEFAULT_H: f32 = 160.0;
/// 图标缺省边长。
const ICON_DEFAULT_PX: f32 = 16.0;
/// 进度条缺省粗细（与系统标准粗条一致）。
const PROGRESS_DEFAULT_GIRTH: f32 = 30.0;

/// 面身份与所在窗口（弹层与图标钮悬停提示都要窗口 id）。
#[derive(Clone)]
struct Ctx {
    plugin_id: String,
    surface_id: String,
    window: iced::window::Id,
}

/// 插件面板内容：取投影缓存里的树渲染；贡献缺失 / 树未提交时给可读占位。
pub fn panel_view<'a>(app: &'a App, window: iced::window::Id, view: &str) -> Element<'a, Message> {
    surface_view(app, window, view, "插件面板")
}

/// 设置页里的插件面（tab 内容）：与面板同一套面树渲染，仅占位文案不同。
pub fn settings_view<'a>(app: &'a App, window: iced::window::Id, view: &str) -> Element<'a, Message> {
    surface_view(app, window, view, "插件设置")
}

/// 插件窗口内容：独立 OS 窗口承载的面树（无面板内边距，铺满窗口）。
pub fn window_view<'a>(
    app: &'a App,
    window: iced::window::Id,
    entry: &crate::plugin_actions::PluginWindowActive,
) -> Element<'a, Message> {
    let key = (entry.plugin_id.clone(), entry.window_key.clone());
    match app.plugin_trees.get(&key) {
        Some(tree) => {
            let ctx = Ctx {
                plugin_id: entry.plugin_id.clone(),
                surface_id: entry.window_key.clone(),
                window,
            };
            container(render_root(app, &ctx, tree))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        None => placeholder(
            "puzzle",
            "插件窗口",
            "等待插件提交界面（插件未运行或尚未就绪）",
        ),
    }
}

/// 接管面内容：与面板同一套面树渲染，无内边距铺满可用区域（接管态由 layout_view 组装）。
pub fn takeover_content<'a>(
    app: &'a App,
    window: iced::window::Id,
    plugin_id: &str,
    surface_id: &str,
) -> Element<'a, Message> {
    let key = (plugin_id.to_string(), surface_id.to_string());
    match app.plugin_trees.get(&key) {
        Some(tree) => {
            let ctx = Ctx {
                plugin_id: plugin_id.to_string(),
                surface_id: surface_id.to_string(),
                window,
            };
            container(render_root(app, &ctx, tree))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        None => placeholder("puzzle", "接管面", "等待插件提交界面"),
    }
}

/// 面内容解释渲染（面板 / 表格视图共用）：按壳侧视图 kind 取插件贡献与树。
pub(crate) fn surface_view<'a>(
    app: &'a App,
    window: iced::window::Id,
    view: &str,
    kind_label: &str,
) -> Element<'a, Message> {
    let Some((plugin_id, surface_id)) = crate::plugin_actions::parse_view_kind(view) else {
        return placeholder("puzzle", kind_label, "视图 kind 非法");
    };
    let Some(contribution) = contribution(app, plugin_id, surface_id) else {
        return placeholder(
            "puzzle",
            &format!("{kind_label} {plugin_id}"),
            "插件不可用：未安装、未启用或已卸载",
        );
    };
    let ctx = Ctx {
        plugin_id: plugin_id.to_string(),
        surface_id: surface_id.to_string(),
        window,
    };
    let key = (ctx.plugin_id.clone(), ctx.surface_id.clone());
    let outer_pad = match contribution.padding {
        Some(pad) => padding_of(pad),
        None => Padding::ZERO.horizontal(12.0).vertical(10.0),
    };
    match app.plugin_trees.get(&key) {
        Some(tree) => container(render_root(app, &ctx, tree))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(outer_pad)
            .into(),
        None => {
            let icon_key =
                crate::plugin_actions::plugin_icon_key(&ctx.plugin_id, contribution.icon.as_deref(), "puzzle");
            placeholder(
                &icon_key,
                &contribution.label,
                "等待插件提交界面（插件未运行或尚未就绪）",
            )
        }
    }
}

fn placeholder<'a>(icon_name: &str, title: &str, desc: &str) -> Element<'a, Message> {
    controls::view_placeholder(icon_name, title.to_string(), desc.to_string())
}

/// 根节点渲染：容器按面区域撑满高度（Fill 子项——画布 / 列表——才有可分配空间；
/// Shrink 根列内的 Fill 子项会塌缩成 0 高）。嵌套容器保持各自声明的尺寸语义。
fn render_root<'a>(app: &'a App, ctx: &Ctx, node: &'a ViewNode) -> Element<'a, Message> {
    match &node.body {
        NodeBody::Column { .. } | NodeBody::Row { .. } | NodeBody::Card { .. } | NodeBody::Stack { .. } => {
            render_node_in(app, ctx, node, true)
        }
        _ => render_node_in(app, ctx, node, false),
    }
}

/// 单节点解释：容器递归渲染子节点；叶子按控件语义取主题 token 与事件绑定。
fn render_node<'a>(app: &'a App, ctx: &Ctx, node: &'a ViewNode) -> Element<'a, Message> {
    render_node_in(app, ctx, node, false)
}

fn render_node_in<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    fill_height: bool,
) -> Element<'a, Message> {
    let element = render_body(app, ctx, node, fill_height);
    let element = finish_node(app, ctx, node, element);
    align_node(element, node, fill_height)
}

/// 节点自身外观：交互外观（底色 + 边框）、尺寸上下限、点击命中与悬停上报。
fn finish_node<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    element: Element<'a, Message>,
) -> Element<'a, Message> {
    let interaction = node.interaction.as_ref();
    // 按钮与图标钮自带底色与按下态：外观交给控件，`interaction` 在其中只决定可用性
    let owns_press = binds_press_itself(&node.body);
    let wants_hover = !owns_press && interaction.is_some_and(|spec| spec.hover.is_some());
    let skin = if owns_press { None } else { current_surface(app, ctx, node).map(|surface| skin_of(&surface)) };
    let size = node.body.size();
    let min = Size::new(
        size.and_then(|size| size.min_width).unwrap_or(0.0),
        size.and_then(|size| size.min_height).unwrap_or(0.0),
    );
    let max = size.and_then(|size| match (size.max_width, size.max_height) {
        (None, None) => None,
        (width, height) => Some(Size::new(
            width.unwrap_or(f32::INFINITY),
            height.unwrap_or(f32::INFINITY),
        )),
    });
    // 装饰层的存在只随树变化（不随悬停进出）：结构抖动会让子项控件态被重建，滚动位置与焦点随之丢失
    let needs_box = (!owns_press && interaction.is_some())
        || min.width > 0.0
        || min.height > 0.0
        || max.is_some();
    let mut element = element;
    if needs_box {
        element = NodeBox::new(element, skin.unwrap_or_default(), min, max).into();
    }

    let enabled = interaction.is_none_or(|spec| spec.interactive());
    // 按钮与图标钮自身承接按下（含按下态外观），此处不再重复挂命中
    let on_press = (enabled && !owns_press).then(|| click(ctx, node)).flatten();
    let wants_hover = enabled && wants_hover;
    if on_press.is_none() && !wants_hover {
        return element;
    }
    let mut area = mouse_area(element);
    if wants_hover {
        area = area
            .on_enter(hover_msg(ctx, &node.id, true))
            .on_exit(hover_msg(ctx, &node.id, false));
    }
    match on_press {
        Some(message) => area.on_press(message).interaction(mouse::Interaction::Pointer).into(),
        None => area.into(),
    }
}

/// 节点当前状态命中的交互外观（禁用 / 激活 / 悬停 / 常态依次取第一个已声明的档位）。
fn current_surface(app: &App, ctx: &Ctx, node: &ViewNode) -> Option<Surface> {
    let spec = node.interaction.as_ref()?;
    let hovered = spec.hover.is_some() && hovered(app, ctx, &node.id);
    spec.appearance(hovered).copied()
}

/// 节点是否接受交互（`interaction.state` 为 `disabled` 时关掉点击与悬停）。
fn interaction_enabled(node: &ViewNode) -> bool {
    node.interaction.as_ref().is_none_or(|spec| spec.interactive())
}

/// 控件自身承接按下的节点（点击由控件自己的按下处理产生）。
fn binds_press_itself(body: &NodeBody) -> bool {
    matches!(body, NodeBody::Button { .. } | NodeBody::IconButton { .. })
}

/// 列 / 卡片的水平摆放（宽度不足父容器时按 `align` 放到左 / 中 / 右）。
fn align_node<'a>(element: Element<'a, Message>, node: &ViewNode, fill_height: bool) -> Element<'a, Message> {
    let align = match &node.body {
        NodeBody::Column { align, .. } | NodeBody::Card { align, .. } => *align,
        _ => None,
    };
    align_block(element, align, fill_height)
}

/// 单节点本体的控件语义（不含交互外观与摆放）。
fn render_body<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    fill_height: bool,
) -> Element<'a, Message> {
    match &node.body {
        NodeBody::Column { gap, padding, cross_align, align: _, size } => {
            let mut col = keyed_column(children(app, ctx, node, Some(Axis::Vertical)))
                .width(column_width(*size))
                .height(column_height(*size, fill_height || grows(node)))
                .spacing(gap.unwrap_or(DEFAULT_GAP));
            if let Some(pad) = padding {
                col = col.padding(padding_of(*pad));
            }
            if let Some(align) = cross_align {
                col = col.align_items(alignment(*align));
            }
            col.into()
        }
        NodeBody::Row { gap, padding, justify, cross_align, size } => {
            let spacing = gap.unwrap_or(DEFAULT_GAP);
            let declared = justify.is_some();
            let justify = justify.unwrap_or(Justify::Start);
            let kids = children_elements(app, ctx, node, Some(Axis::Horizontal));
            // spaceBetween：在占满宽度的行里插入弹性空档把子项推向两端
            let mut line = if justify == Justify::SpaceBetween {
                let mut els = Vec::new();
                for (index, child) in kids.into_iter().enumerate() {
                    if index > 0 {
                        els.push(space().width(Length::Fill).into());
                    }
                    els.push(child);
                }
                row(els)
            } else {
                row(kids)
            }
            .spacing(spacing)
            .align_y(cross_align.map(vertical).unwrap_or(Vertical::Center));
            // 占满宽度是「按权重分配子项」与「两端对齐」的前提：行自身收缩就没有可分配的剩余空间
            let fill_width = grows(node) || justify == Justify::SpaceBetween;
            let width = axis_length(
                size.and_then(|size| size.width),
                if fill_width { Length::Fill } else { Length::Shrink },
            );
            line = line
                .width(width)
                .height(axis_length(size.and_then(|size| size.height), if fill_height {
                    Length::Fill
                } else {
                    Length::Shrink
                }));
            if let Some(pad) = padding {
                line = line.padding(padding_of(*pad));
            }
            // 声明了 justify 才谈得上主轴分布（spaceBetween 行自身已撑满，其余套占满宽度的容器）；
            // 未声明时保持 row 原样——宽度收缩，随父列的 cross_align 对齐
            if !declared || justify == Justify::SpaceBetween {
                line.into()
            } else {
                container(line)
                    .width(Length::Fill)
                    .align_x(justify_x(justify))
                    .into()
            }
        }
        NodeBody::Card { gap, padding, accent, tone, radius, bordered, align: _, size } => {
            let inner = keyed_column(children(app, ctx, node, Some(Axis::Vertical)))
                .width(column_width(*size))
                .height(Length::Shrink)
                .spacing(gap.unwrap_or(DEFAULT_GAP));
            // 交互外观接管底色与边框时自身不再绘制（避免两层底色互相盖住）；
            // 当前状态没有对应档位时仍按卡片自己的档位绘制，避免整张卡片消失
            let skin_paints = current_surface(app, ctx, node).is_some();
            let style = if skin_paints {
                container::Style::default()
            } else {
                card_style(accent.unwrap_or_default(), *tone, *radius, *bordered)
            };
            let mut card = container(inner)
                .width(column_width(*size))
                .padding(padding_of(padding.unwrap_or(PaddingSpec::All(10.0))))
                .style(move |_t| style);
            if let Some(height) = card_height(*size, fill_height) {
                card = card.height(height);
            }
            card.into()
        }
        NodeBody::Stack {} => {
            let mut stack = Stack::new();
            for child in children_elements(app, ctx, node, None) {
                stack = stack.push(child);
            }
            let element: Element<'a, Message> = stack.into();
            if fill_height {
                container(element).width(Length::Fill).height(Length::Fill).into()
            } else {
                element
            }
        }
        NodeBody::Scroll { direction, gap, height } => {
            let spacing = gap.unwrap_or(DEFAULT_GAP);
            let axis = direction.unwrap_or(Axis::Vertical);
            let inner: Element<Message> = match axis {
                // 纵向内容撑满视口宽度（自适应换行）；横向内容按自身宽度溢出
                Axis::Horizontal => row(children_elements(app, ctx, node, None)).spacing(spacing).into(),
                Axis::Vertical => column(children_elements(app, ctx, node, None))
                    .width(Length::Fill)
                    .spacing(spacing)
                    .into(),
            };
            let mut scroll = match axis {
                Axis::Horizontal => scroll::h(inner),
                Axis::Vertical => scroll::v(inner),
            }
            .id(node_scroll_id(ctx, &node.id))
            .width(Length::Fill)
            .height(height.map_or(Length::Fill, Length::Fixed));
            if declares(node, EventKind::Scroll) {
                scroll = scroll.on_scroll(scroll_report(ctx, &node.id, axis));
            }
            scroll.into()
        }
        NodeBody::Grid { columns, min_column_width, gap, row_height } => grid_node(
            app,
            ctx,
            node,
            *columns,
            *min_column_width,
            gap.unwrap_or(DEFAULT_GAP),
            *row_height,
        ),
        NodeBody::List { gap } => {
            let mut list = scroll::v(
                keyed_column(children(app, ctx, node, None))
                    .width(Length::Fill)
                    .height(Length::Shrink)
                    .spacing(gap.unwrap_or(DEFAULT_GAP)),
            )
            .id(node_scroll_id(ctx, &node.id))
            .width(Length::Fill)
            .height(Length::Fill);
            if declares(node, EventKind::Scroll) {
                list = list.on_scroll(scroll_report(ctx, &node.id, Axis::Vertical));
            }
            list.into()
        }
        NodeBody::Divider {} => controls::h_divider_subtle(),
        NodeBody::Text { content, size, weight, color, align, family, wrap } => {
            let mut line = text(content.as_str())
                .size(pal::fs_body() * size.unwrap_or(1.0))
                .font(text_font(*weight, *family))
                .color(color.map_or(pal::TEXT_PRIMARY, spec_color));
            if *wrap == Some(false) {
                line = line.wrapping(iced::advanced::text::Wrapping::None);
            }
            // 声明对齐才占满父容器宽度（对齐只在该前提下有意义）；否则按内容宽度收缩，
            // 让 shrink 容器能形成内容自适应气泡
            match align {
                Some(align) => container(line)
                    .width(Length::Fill)
                    .align_x(horizontal(*align))
                    .into(),
                None => line.into(),
            }
        }
        NodeBody::Markdown { size, weight, color, family, .. } => {
            let key = (ctx.plugin_id.clone(), ctx.surface_id.clone(), node.id.clone());
            let font = (*weight)
                .map(|weight| text_font(Some(weight), *family))
                .or_else(|| family.map(|family| text_font(None, Some(family))));
            match app.plugin_markdown.get(&key) {
                Some(entry) => crate::markdown_view::document_body_styled(
                    &entry.doc,
                    &app.theme,
                    &app.note_images,
                    app.global.soft_wrap,
                    size.unwrap_or(1.0),
                    crate::markdown_view::BodyStyle {
                        font,
                        color: color.map(spec_color),
                        link_scope: Some(crate::markdown_view::PluginLinkScope {
                            plugin_id: ctx.plugin_id.clone(),
                            surface_id: ctx.surface_id.clone(),
                            node_id: node.id.clone(),
                        }),
                    },
                ),
                None => text("").into(),
            }
        }
        NodeBody::TextInput { placeholder, multiline, password, read_only, .. } => {
            let placeholder = placeholder.as_deref().unwrap_or_default();
            if read_only.unwrap_or(false) {
                let value = node_value_text(&node.body).unwrap_or_default();
                read_only_field(&value, placeholder, password.unwrap_or(false), multiline.unwrap_or(false))
            } else if multiline.unwrap_or(false) {
                multiline_input(app, ctx, node, placeholder)
            } else {
                single_line_input(app, ctx, node, placeholder, password.unwrap_or(false))
            }
        }
        NodeBody::NumberInput { placeholder, read_only, .. } => {
            let placeholder = placeholder.as_deref().unwrap_or_default();
            if read_only.unwrap_or(false) {
                let value = node_value_text(&node.body).unwrap_or_default();
                read_only_field(&value, placeholder, false, false)
            } else {
                single_line_input(app, ctx, node, placeholder, false)
            }
        }
        NodeBody::Dropdown { options, selected, placeholder, read_only } => {
            let label = selected
                .and_then(|index| options.get(index as usize))
                .cloned()
                .or_else(|| placeholder.clone())
                .unwrap_or_else(|| "请选择".to_string());
            let entries = options
                .iter()
                .enumerate()
                .map(|(index, option)| PopupEntry {
                    label: option.clone(),
                    current: *selected == Some(index as u32),
                    disabled: false,
                    danger: false,
                    on_press: node_event(ctx, &node.id, ViewEvent::Change { value: EventValue::Number(index as f64) }),
                })
                .collect();
            let open = (declares(node, EventKind::Change) && !read_only.unwrap_or(false))
                .then(|| Message::PluginList {
                    window: ctx.window,
                    entries,
                });
            controls::dropdown_trigger(&label, Length::Fill, open)
        }
        NodeBody::Checkbox { checked, label, read_only } => {
            let state = if *checked { controls::CheckState::On } else { controls::CheckState::Off };
            let on_press = (declares(node, EventKind::Change) && !read_only.unwrap_or(false)).then(|| {
                node_event(ctx, &node.id, ViewEvent::Change { value: EventValue::Bool(!checked) })
            });
            match label {
                Some(label) => row![
                    controls::checkbox(state, on_press),
                    text(label.as_str()).size(pal::fs_ui()).color(pal::TEXT_PRIMARY),
                ]
                .spacing(6)
                .align_y(Vertical::Center)
                .into(),
                None => controls::checkbox(state, on_press),
            }
        }
        NodeBody::Button { label, icon, variant, enabled, size } => {
            let variant = button_variant(*variant);
            let on_press = (enabled.unwrap_or(true) && interaction_enabled(node))
                .then(|| click(ctx, node))
                .flatten();
            match icon {
                Some(name) => {
                    let glyph = icon_element(ctx, name, 14.0, controls::variant_text_color(variant));
                    controls::btn_with_icon(label.as_str(), glyph, &button_size(*size), variant, on_press).into()
                }
                None => controls::btn(label.as_str(), &button_size(*size), variant, on_press).into(),
            }
        }
        NodeBody::IconButton { icon, tooltip, enabled, size, variant, hover_only } => {
            let (icon_px, box_px) = icon_box(*size);
            let variant = icon_variant(*variant);
            let on_press = (enabled.unwrap_or(true) && interaction_enabled(node))
                .then(|| click(ctx, node))
                .flatten();
            let tip = tooltip.as_ref().map(|label| (ctx.window, label.clone()));
            let icon_el = icon_element(ctx, icon, icon_px, controls::variant_text_color(variant));
            if hover_only.unwrap_or(false) {
                // 悬停才显示：未悬停时占位同尺寸（布局不跳），进出由覆盖热区上报
                let shown = hovered(app, ctx, &node.id);
                let inner: Element<Message> = if shown {
                    controls::icon_button_el(icon_el, box_px, variant, None, tip)
                } else {
                    container(space())
                        .width(Length::Fixed(box_px))
                        .height(Length::Fixed(box_px))
                        .into()
                };
                let mut area = mouse_area(inner)
                    .on_enter(hover_msg(ctx, &node.id, true))
                    .on_exit(hover_msg(ctx, &node.id, false));
                if let Some(msg) = on_press {
                    area = area.on_press(msg);
                }
                area.into()
            } else {
                controls::icon_button_el(icon_el, box_px, variant, on_press, tip)
            }
        }
        NodeBody::Icon { name, size, color } => {
            let color = color.map_or(pal::TEXT_SECONDARY, spec_color);
            icon_element(ctx, name, size.unwrap_or(ICON_DEFAULT_PX), color)
        }
        NodeBody::Image { source, fit, width, height } => image_node(
            app,
            &ctx.plugin_id,
            source,
            fit.unwrap_or(ImageFit::Contain),
            *width,
            *height,
        ),
        NodeBody::Progress { value, girth, tone } => {
            let thickness = girth.unwrap_or(PROGRESS_DEFAULT_GIRTH).max(1.0);
            let bar_color = tone.map_or(pal::ACCENT, spec_color);
            progress_bar(0.0..=1.0, value.clamp(0.0, 1.0) as f32)
                .length(Length::Fill)
                .girth(Length::Fixed(thickness))
                .style(move |_theme| progress_bar::Style {
                    background: pal::BG_SUNKEN.into(),
                    bar: bar_color.into(),
                    border: iced::border::rounded(thickness / 2.0),
                })
                .into()
        }
        NodeBody::Menu { items, label, icon } => {
            let entries = items
                .iter()
                .map(|item| PopupEntry {
                    label: item.label.clone(),
                    current: item.current.unwrap_or(false),
                    disabled: item.disabled.unwrap_or(false),
                    danger: item.danger.unwrap_or(false),
                    on_press: node_event(ctx, &node.id, ViewEvent::Change { value: EventValue::Text(item.action.clone()) }),
                })
                .collect();
            let open = declares(node, EventKind::Change).then(|| Message::PluginList {
                window: ctx.window,
                entries,
            });
            match (label, icon) {
                // 图标 + 文字并存：紧凑的「图标 + 文字」触发钮（非全宽下拉钮）
                (Some(label), Some(icon)) => {
                    let icon_el = icon_element(ctx, icon, 14.0, pal::TEXT_SECONDARY);
                    row![icon_el, controls::dropdown_trigger(label, Length::Shrink, open)]
                        .spacing(4)
                        .align_y(Vertical::Center)
                        .into()
                }
                (Some(label), None) => controls::dropdown_trigger(label, Length::Fill, open),
                (None, Some(icon)) => {
                    let icon_el = icon_element(ctx, icon, 14.0, controls::variant_text_color(Variant::Ghost));
                    controls::icon_button_el(icon_el, controls::ICON_MD, Variant::Ghost, open, None)
                }
                (None, None) => controls::dropdown_trigger("…", Length::Fill, open),
            }
        }
        NodeBody::Canvas { mouse_events, height, fill } => {
            let canvas: Element<Message> = crate::plugin_canvas::PluginCanvas {
                app,
                window: ctx.window,
                plugin_id: ctx.plugin_id.clone(),
                surface_id: ctx.surface_id.clone(),
                node_id: &node.id,
                list: app
                    .plugin_canvases
                    .get(&(ctx.plugin_id.clone(), ctx.surface_id.clone(), node.id.clone())),
                mouse_events,
            }
            .into();
            let height = if fill.unwrap_or(false) {
                Length::Fill
            } else {
                Length::Fixed(height.unwrap_or(CANVAS_DEFAULT_H))
            };
            container(canvas).width(Length::Fill).height(height).into()
        }
        NodeBody::DragRegion {} => iced::widget::mouse_area(
            container(space())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::PluginWindowDrag(ctx.window))
        .into(),
    }
}

/// 子节点渲染（键 = 节点 id 哈希：插件整树重发时纵向容器按 id 复用控件态，输入焦点与光标不丢）。
/// 横向容器不做按键（iced 无按键行），其控件态依赖结构稳定。
fn children<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    axis: Option<Axis>,
) -> Vec<(u64, Element<'a, Message>)> {
    node.children
        .iter()
        .map(|child| (node_key(&child.id), child_element(app, ctx, child, axis)))
        .collect()
}

/// 子节点元素序列（行 / 滚动 / 网格容器用）。
fn children_elements<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    axis: Option<Axis>,
) -> Vec<Element<'a, Message>> {
    node.children.iter().map(|child| child_element(app, ctx, child, axis)).collect()
}

/// 子节点元素：声明 `grow` 的子项按权重占父容器主轴剩余空间（交叉轴由子项自身尺寸决定）。
fn child_element<'a>(
    app: &'a App,
    ctx: &Ctx,
    child: &'a ViewNode,
    axis: Option<Axis>,
) -> Element<'a, Message> {
    let element = render_node(app, ctx, child);
    match (child.grow, axis) {
        (Some(portion), Some(Axis::Horizontal)) => {
            container(element).width(Length::FillPortion(portion)).into()
        }
        (Some(portion), Some(Axis::Vertical)) => {
            container(element).height(Length::FillPortion(portion)).into()
        }
        _ => element,
    }
}

/// 子项是否声明了弹性权重（决定父容器主轴须占满才分配得了剩余空间）。
fn grows(node: &ViewNode) -> bool {
    node.children.iter().any(|child| child.grow.is_some())
}

/// 列与卡片缺省占满宽度。
fn column_width(size: Option<BoxSize>) -> Length {
    axis_length(size.and_then(|size| size.width), Length::Fill)
}

/// 列与卡片的高度（缺省按内容；根节点与带权重子项的容器撑满）。
fn column_height(size: Option<BoxSize>, fill_height: bool) -> Length {
    axis_length(size.and_then(|size| size.height), if fill_height {
        Length::Fill
    } else {
        Length::Shrink
    })
}

/// 卡片高度：未声明且不需要撑满时不设（保持容器缺省按内容）。
fn card_height(size: Option<BoxSize>, fill_height: bool) -> Option<Length> {
    match size.and_then(|size| size.height) {
        Some(spec) => Some(axis_length(Some(spec), Length::Shrink)),
        None if fill_height => Some(Length::Fill),
        None => None,
    }
}

/// 单轴尺寸声明 → iced 长度（缺省取 `fallback`）。
fn axis_length(spec: Option<SizeSpec>, fallback: Length) -> Length {
    match spec {
        Some(SizeSpec::Fill) => Length::Fill,
        Some(SizeSpec::Shrink) => Length::Shrink,
        Some(SizeSpec::Fixed(value)) => Length::Fixed(value.max(0.0)),
        None => fallback,
    }
}

/// 内边距声明 → iced 内边距（四边取值）。
fn padding_of(spec: PaddingSpec) -> Padding {
    let [top, right, bottom, left] = spec.edges();
    Padding { top, right, bottom, left }
}

/// 一层交互外观 → 容器样式（底色 + 1px 描边 + 圆角）。
fn skin_of(surface: &Surface) -> container::Style {
    let radius = surface.radius.unwrap_or(pal::RADIUS_SM).max(0.0);
    let border = match surface.border {
        Some(color) => iced::border::rounded(radius).color(spec_color(color)).width(1.0),
        None => iced::border::rounded(radius),
    };
    container::Style {
        background: surface.background.map(|color| spec_color(color).into()),
        border,
        ..container::Style::default()
    }
}

/// 容器宽度小于父容器时的水平摆放（声明 `align` 才套占满宽度的外层）。
fn align_block<'a>(
    element: Element<'a, Message>,
    align: Option<Align>,
    fill_height: bool,
) -> Element<'a, Message> {
    match align {
        Some(align) => {
            let mut wrap = container(element)
                .width(Length::Fill)
                .align_x(horizontal(align));
            if fill_height {
                wrap = wrap.height(Length::Fill);
            }
            wrap.into()
        }
        None => element,
    }
}

/// 滚动位置回发（主轴偏移 + 可滚动余量）；相同视口不重复发布由控件自身保证。
fn scroll_report<'a>(
    ctx: &Ctx,
    node_id: &str,
    axis: Axis,
) -> impl Fn(iced::widget::scrollable::Viewport) -> Message + 'a {
    let plugin_id = ctx.plugin_id.clone();
    let surface_id = ctx.surface_id.clone();
    let node_id = node_id.to_string();
    move |viewport| {
        let bounds = viewport.bounds();
        let content = viewport.content_bounds();
        let offset = viewport.absolute_offset();
        let (offset, max_offset) = match axis {
            Axis::Horizontal => (offset.x, (content.width - bounds.width).max(0.0)),
            Axis::Vertical => (offset.y, (content.height - bounds.height).max(0.0)),
        };
        Message::PluginView {
            plugin_id: plugin_id.clone(),
            surface_id: surface_id.clone(),
            node_id: node_id.clone(),
            event: ViewEvent::Scroll { offset, max_offset },
        }
    }
}

/// 网格：`columns` 固定列数；`min_column_width` 按可用宽度换列（`columns` 同时给定时作列数上限）。
/// `row_height` 缺省按列宽 1:1 定行高，`content` 让每行按内容高度。
fn grid_node<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    columns: Option<u32>,
    min_column_width: Option<f32>,
    gap: f32,
    row_height: Option<RowHeight>,
) -> Element<'a, Message> {
    let count = node.children.len().max(1);
    let fixed = columns.map(|columns| columns.max(1) as usize);
    let build = move |app: &'a App, ctx: &Ctx, columns: usize| -> Element<'a, Message> {
        let mut grid = grid(children_elements(app, ctx, node, Some(Axis::Horizontal)))
            .columns(columns)
            .spacing(gap);
        match row_height {
            // 内容高度：每行按自身内容（缺省按列宽 1:1，短内容会被拉成方格）
            Some(RowHeight::Content) => grid = grid.height(Length::Shrink),
            // 固定行高：网格把可用高度均分给各行，故按行数换算成整网格高度
            Some(RowHeight::Fixed(value)) => {
                let rows = count.div_ceil(columns.max(1));
                let total = value.max(0.0) * rows as f32 + gap * rows.saturating_sub(1) as f32;
                grid = grid.height(Length::Fixed(total));
            }
            None => {}
        }
        grid.into()
    };
    let Some(min_column_width) = min_column_width else {
        return build(app, ctx, fixed.unwrap_or(1));
    };
    let min_column_width = min_column_width.max(1.0);
    // 列数随可用宽度变化：布局期取实际宽度算列，固定列数只作换列上限。
    // 面身份按值进闭包——元素本身不借它，避免把整棵树的返回寿命绑到局部 `ctx`。
    let ctx = ctx.clone();
    responsive(move |size| {
        let fit = ((size.width + gap) / (min_column_width + gap)).floor().max(1.0) as usize;
        let columns = match fixed {
            Some(fixed) => fit.min(fixed),
            None => fit.min(count),
        };
        build(app, &ctx, columns)
    })
    .height(Length::Shrink)
    .into()
}

/// 表单框外观（多行输入与只读多行共用：同款底色、边框、圆角）。
fn form_box_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: iced::border::rounded(pal::RADIUS_SM).color(pal::BORDER_STRONG).width(1.0),
        ..container::Style::default()
    }
}

/// 只读输入：保留表单外观但不接受编辑（单行用禁用态输入框，多行用同款静态框）。
fn read_only_field<'a>(
    value: &str,
    placeholder: &str,
    password: bool,
    multiline: bool,
) -> Element<'a, Message> {
    if !multiline {
        return text_input(placeholder, value)
            .size(pal::fs_ui())
            .font(crate::ui::regular())
            .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
            .width(Length::Fill)
            .secure(password)
            .style(|_theme, status| controls::form_input_style(status))
            .into();
    }
    let content = if value.is_empty() {
        text(placeholder.to_string()).size(pal::fs_ui()).color(pal::INPUT_PLACEHOLDER)
    } else {
        text(value.to_string()).size(pal::fs_ui()).color(pal::TEXT_PRIMARY)
    };
    // 固定高度内可滚动：长文不裁切、也不把后面的节点挤出可视区
    container(scroll::v(content).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fixed(EDITOR_H))
        .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
        .style(form_box_style)
        .into()
}

/// 节点 id → 容器子项键（`keyed_column` 要求 `Copy + PartialEq`，故用 64 位哈希）。
fn node_key(id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 节点事件消息（面身份随事件一起回发）。
fn node_event(ctx: &Ctx, node_id: &str, event: ViewEvent) -> Message {
    Message::PluginView {
        plugin_id: ctx.plugin_id.clone(),
        surface_id: ctx.surface_id.clone(),
        node_id: node_id.to_string(),
        event,
    }
}

/// 点击事件绑定（节点声明了 click 才挂处理器）。
fn click(ctx: &Ctx, node: &ViewNode) -> Option<Message> {
    declares(node, EventKind::Click).then(|| node_event(ctx, &node.id, ViewEvent::Click))
}

/// 单行 / 数字输入（草稿值决定显示；键入发 change，Enter 发 submit）。
fn single_line_input<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    placeholder: &str,
    password: bool,
) -> Element<'a, Message> {
    let plugin_value = node_value_text(&node.body).unwrap_or_default();
    let key = (ctx.plugin_id.clone(), ctx.surface_id.clone(), node.id.clone());
    let value = input_display(&plugin_value, app.plugin_drafts.get(&key));
    let input = text_input(placeholder, value.as_str())
        .size(pal::fs_ui())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
        .width(Length::Fill)
        .secure(password)
        .style(|_theme, status| controls::form_input_style(status))
        .on_input({
            let ctx = Ctx { plugin_id: ctx.plugin_id.clone(), surface_id: ctx.surface_id.clone(), window: ctx.window };
            let node_id = node.id.clone();
            move |text| node_event(&ctx, &node_id, ViewEvent::Change { value: EventValue::Text(text) })
        });
    if declares(node, EventKind::Submit) {
        input
            .on_submit(node_event(ctx, &node.id, ViewEvent::Submit { value: EventValue::Text(value) }))
            .into()
    } else {
        input.into()
    }
}

/// 多行输入（编辑器状态归宿主：插件不改值则光标与选区原地保留）。
fn multiline_input<'a>(
    app: &'a App,
    ctx: &Ctx,
    node: &'a ViewNode,
    placeholder: &'a str,
) -> Element<'a, Message> {
    let key = (ctx.plugin_id.clone(), ctx.surface_id.clone(), node.id.clone());
    let Some(state) = app.plugin_editors.get(&key) else {
        // 编辑器随树投影建立，首帧尚未就绪时按空输入呈现
        return container(text(String::new()))
            .width(Length::Fill)
            .height(Length::Fixed(EDITOR_H))
            .style(form_box_style)
            .into();
    };
    let (plugin_id, surface_id, node_id) = key;
    text_editor(&state.content)
        .placeholder(placeholder)
        .size(pal::fs_ui())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(INPUT_PAD_X).vertical(INPUT_PAD_Y))
        .height(Length::Fixed(EDITOR_H))
        .style(|_theme, _status| text_editor::Style {
            background: pal::BG_TERTIARY.into(),
            border: iced::border::rounded(pal::RADIUS_SM).color(pal::BORDER_STRONG).width(1.0),
            placeholder: pal::INPUT_PLACEHOLDER,
            value: pal::TEXT_PRIMARY,
            selection: pal::SELECTION,
        })
        .on_action(move |action| Message::PluginTextEdit {
            plugin_id: plugin_id.clone(),
            surface_id: surface_id.clone(),
            node_id: node_id.clone(),
            action,
        })
        .into()
}

/// 界面图片：句柄已载入则呈现，否则给可读占位（载入任务由 `plugin_actions::refresh_projection` 发起）。
/// 尺寸缺省按原图；`fill` 占满父容器该轴。
fn image_node<'a>(
    app: &'a App,
    plugin_id: &str,
    source: &ImageSource,
    fit: ImageFit,
    width: Option<SizeSpec>,
    height: Option<SizeSpec>,
) -> Element<'a, Message> {
    let handle = image_key(app, plugin_id, source).and_then(|key| app.note_images.get(&key).cloned());
    match handle {
        Some(handle) => {
            let mut widget = image(handle).content_fit(content_fit(fit));
            if let Some(width) = width.map(axis_length_of) {
                widget = widget.width(width);
            }
            if let Some(height) = height.map(axis_length_of) {
                widget = widget.height(height);
            }
            widget.into()
        }
        None => placeholder("image", "图片", "加载中或不可用"),
    }
}

fn axis_length_of(spec: SizeSpec) -> Length {
    axis_length(Some(spec), Length::Shrink)
}

/// 自备图标名 → 图标元素：内置形状名直接命中，否则按 `<插件 id>::<名>` 取自备图形。
/// `Ctx` 的插件身份决定命名空间，故同名图形不跨插件串用。
fn icon_element<'a>(ctx: &Ctx, name: &str, px: f32, color: Color) -> Element<'a, Message> {
    let key = icon_key(ctx, name);
    crate::ui::icon::icon_resolved(&key, px, color, 1.5)
}

/// 图标解析键：内置名原样，未知名按插件命名空间拼限定名（取自备图形，缺省画空）。
fn icon_key(ctx: &Ctx, name: &str) -> String {
    if crate::ui::icon::is_builtin(name) {
        return name.to_string();
    }
    format!("{}::{}", ctx.plugin_id, name)
}

/// 文字按钮尺寸档（缺省 `Sm`）。
fn button_size(size: Option<ControlSize>) -> controls::TextSize {
    match size {
        Some(ControlSize::Md) => controls::btn_md(),
        Some(ControlSize::Lg) => controls::btn_lg(),
        _ => controls::btn_sm(),
    }
}

/// 图标钮的（图标边长, 方框边长）档（缺省 `Sm`：14 / 28）。
fn icon_box(size: Option<ControlSize>) -> (f32, f32) {
    match size {
        Some(ControlSize::Md) => (16.0, controls::ICON_LG),
        Some(ControlSize::Lg) => (20.0, controls::ICON_XL),
        _ => (14.0, controls::ICON_MD),
    }
}

/// 卡片样式：底色按档位、边框可关、圆角可调。
fn card_style(
    accent: bool,
    tone: Option<CardTone>,
    radius: Option<f32>,
    bordered: Option<bool>,
) -> container::Style {
    let background = match tone.unwrap_or(CardTone::Default) {
        CardTone::Default => Some(pal::BG_TERTIARY.into()),
        CardTone::Muted => Some(pal::BG_SECONDARY.into()),
        CardTone::AccentSoft => Some(pal::ACCENT_SOFT.into()),
        CardTone::Transparent => None,
    };
    let border = if bordered.unwrap_or(true) {
        iced::border::rounded(radius.unwrap_or(pal::RADIUS_SM))
            .color(if accent { pal::ACCENT } else { pal::BORDER })
            .width(1.0)
    } else {
        iced::border::rounded(radius.unwrap_or(pal::RADIUS_SM)).color(Color::TRANSPARENT).width(0.0)
    };
    container::Style { background, border, ..container::Style::default() }
}

/// 可滚动节点的稳定 id（插件 id + 面 id + 节点 id；`view.scrollTo` 按它定位）。
pub(crate) fn scroll_id(plugin_id: &str, surface_id: &str, node_id: &str) -> String {
    format!("plugin-scroll:{plugin_id}:{surface_id}:{node_id}")
}

fn node_scroll_id(ctx: &Ctx, node_id: &str) -> String {
    scroll_id(&ctx.plugin_id, &ctx.surface_id, node_id)
}

/// 某节点的悬停态（交互外观与 `hoverOnly` 图标钮据此切换）。
fn hovered(app: &App, ctx: &Ctx, node_id: &str) -> bool {
    app.plugin_hovered.contains(&(ctx.plugin_id.clone(), ctx.surface_id.clone(), node_id.to_string()))
}

/// 悬停进出消息。
fn hover_msg(ctx: &Ctx, node_id: &str, entered: bool) -> Message {
    Message::PluginHover {
        plugin_id: ctx.plugin_id.clone(),
        surface_id: ctx.surface_id.clone(),
        node_id: node_id.to_string(),
        entered,
    }
}

fn justify_x(justify: Justify) -> Horizontal {
    match justify {
        Justify::Start | Justify::SpaceBetween => Horizontal::Left,
        Justify::Center => Horizontal::Center,
        Justify::End => Horizontal::Right,
    }
}

/// 插件声明的取色（主题 token 或自定义色）。
fn spec_color(spec: ColorSpec) -> Color {
    crate::plugin_canvas::spec_color(spec)
}

/// 字体族与字重 → 字体（等宽族始终取系统等宽字体，不随界面字体族设置变化）。
fn text_font(weight: Option<TextWeight>, family: Option<TextFamily>) -> iced::Font {
    let base = match family {
        Some(TextFamily::Mono) => crate::ui::font::mono(),
        _ => crate::ui::regular(),
    };
    iced::Font {
        weight: weight.map(weight_of).unwrap_or(iced::font::Weight::Normal),
        ..base
    }
}

fn weight_of(weight: TextWeight) -> iced::font::Weight {
    match weight {
        TextWeight::Normal => iced::font::Weight::Normal,
        TextWeight::Medium => iced::font::Weight::Medium,
        TextWeight::SemiBold => iced::font::Weight::Semibold,
    }
}

/// 按钮档位映射（缺省 `Secondary`）。
fn button_variant(variant: Option<ButtonVariant>) -> Variant {
    match variant {
        None => Variant::Secondary,
        Some(ButtonVariant::Primary) => Variant::Primary,
        Some(ButtonVariant::Ghost) => Variant::Ghost,
        Some(ButtonVariant::Danger) => Variant::Danger,
    }
}

/// 图标钮档位映射：缺省 `Ghost`（透明底、悬停才出底——工具条图标钮的常态）。
fn icon_variant(variant: Option<ButtonVariant>) -> Variant {
    match variant {
        None => Variant::Ghost,
        Some(ButtonVariant::Primary) => Variant::Primary,
        Some(ButtonVariant::Ghost) => Variant::Ghost,
        Some(ButtonVariant::Danger) => Variant::Danger,
    }
}

fn content_fit(fit: ImageFit) -> iced::ContentFit {
    match fit {
        ImageFit::Contain => iced::ContentFit::Contain,
        ImageFit::Cover => iced::ContentFit::Cover,
        ImageFit::Fill => iced::ContentFit::Fill,
    }
}

fn horizontal(align: Align) -> Horizontal {
    match align {
        Align::Start => Horizontal::Left,
        Align::Center => Horizontal::Center,
        Align::End => Horizontal::Right,
    }
}

fn alignment(align: Align) -> iced::Alignment {
    match align {
        Align::Start => iced::Alignment::Start,
        Align::Center => iced::Alignment::Center,
        Align::End => iced::Alignment::End,
    }
}

fn vertical(align: Align) -> Vertical {
    match align {
        Align::Start => Vertical::Top,
        Align::Center => Vertical::Center,
        Align::End => Vertical::Bottom,
    }
}

/// 节点外观装饰：在子项矩形内绘制底色与边框、按上下限钳制尺寸，布局完全委托子项。
///
/// 不能改用 `container` 包一层：容器自带交叉轴收缩语义，套在 `Fill` 子项或滚动容器上会把布局
/// 压塌。这里的 `layout` 只把子项的尺寸抬到最小、把上限压进 limits，其余照搬子项结果。
struct NodeBox<'a, Message> {
    inner: Element<'a, Message>,
    skin: container::Style,
    min: Size,
    max: Option<Size>,
}

impl<'a, Message> NodeBox<'a, Message> {
    fn new(inner: Element<'a, Message>, skin: container::Style, min: Size, max: Option<Size>) -> Self {
        Self { inner, skin, min, max }
    }
}

impl<'a, Message: Clone + 'static> Widget<Message, Theme, Renderer> for NodeBox<'a, Message> {
    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.inner.as_widget().size_hint()
    }

    fn state(&self) -> tree::State {
        tree::State::new(())
    }

    fn children(&self) -> Vec<tree::Tree> {
        vec![tree::Tree::new(self.inner.as_widget())]
    }

    fn diff(&self, tree: &mut tree::Tree) {
        tree.diff_children(std::slice::from_ref(&self.inner));
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &Renderer,
        limits: &Limits,
    ) -> Node {
        let limits = match self.max {
            Some(max) => limits.max_width(max.width).max_height(max.height),
            None => *limits,
        };
        let child = self.inner.as_widget_mut().layout(&mut tree.children[0], renderer, &limits);
        let size = child.size();
        let size = Size::new(
            size.width.max(self.min.width),
            size.height.max(self.min.height),
        );
        Node::with_children(size, vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut tree::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget_mut().operate(&mut tree.children[0], child, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        container::draw_background(renderer, &self.skin, layout.bounds());
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget().draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &tree::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let child = layout.children().next().expect("装饰节点必有子布局");
        self.inner.as_widget().mouse_interaction(&tree.children[0], child, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut tree::Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let child = layout.children().next()?;
        self.inner
            .as_widget_mut()
            .overlay(&mut tree.children[0], child, renderer, viewport, translation)
    }
}

impl<'a, Message: Clone + 'static> From<NodeBox<'a, Message>> for Element<'a, Message> {
    fn from(node: NodeBox<'a, Message>) -> Self {
        Element::new(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_core::plugin_runtime::{AxesPadding, EdgesPadding, ColorToken};

    #[test]
    fn node_keys_are_stable_and_distinct() {
        assert_eq!(node_key("status"), node_key("status"), "同一节点 id 的键必须稳定");
        assert_ne!(node_key("status"), node_key("status2"));
    }

    #[test]
    fn axis_length_maps_specs_and_falls_back() {
        assert_eq!(axis_length(None, Length::Shrink), Length::Shrink);
        assert_eq!(axis_length(Some(SizeSpec::Fill), Length::Shrink), Length::Fill);
        assert_eq!(axis_length(Some(SizeSpec::Shrink), Length::Fill), Length::Shrink);
        assert_eq!(axis_length(Some(SizeSpec::Fixed(24.0)), Length::Fill), Length::Fixed(24.0));
        // 负值按 0 处理，不产生反向尺寸
        assert_eq!(axis_length(Some(SizeSpec::Fixed(-4.0)), Length::Fill), Length::Fixed(0.0));
    }

    #[test]
    fn padding_spec_maps_all_four_edges() {
        let axes = padding_of(PaddingSpec::Axes(AxesPadding {
            horizontal: 2.0,
            vertical: 1.0,
        }));
        assert_eq!(axes, Padding { top: 1.0, right: 2.0, bottom: 1.0, left: 2.0 });
        let edges = padding_of(PaddingSpec::Edges(EdgesPadding {
            top: 1.0,
            right: 2.0,
            bottom: 3.0,
            left: 4.0,
        }));
        assert_eq!(edges, Padding { top: 1.0, right: 2.0, bottom: 3.0, left: 4.0 });
    }

    #[test]
    fn skin_carries_background_border_and_radius() {
        let skin = skin_of(&Surface {
            background: Some(ColorSpec::Token(ColorToken::AccentSoft)),
            border: Some(ColorSpec::Token(ColorToken::Accent)),
            radius: Some(8.0),
        });
        assert!(matches!(skin.background, Some(iced::Background::Color(color)) if color == pal::ACCENT_SOFT));
        assert_eq!(skin.border.width, 1.0);
        assert_eq!(skin.border.color, pal::ACCENT);
        assert_eq!(skin.border.radius.top_left, 8.0);
    }

    #[test]
    fn weight_maps_to_real_font_weights() {
        assert_eq!(weight_of(TextWeight::Medium), iced::font::Weight::Medium);
        assert_eq!(weight_of(TextWeight::SemiBold), iced::font::Weight::Semibold);
        // 等宽族：换族但不动字重
        let mono = text_font(Some(TextWeight::Normal), Some(TextFamily::Mono));
        assert!(matches!(mono.family, iced::font::Family::Monospace));
    }
}
