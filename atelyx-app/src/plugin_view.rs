//! 插件面板渲染：把 [`ViewNode`] 树解释成 iced 控件（布局容器 / 文本 / markdown / 输入 / 按钮 /
//! 列表 / 图片 / 进度条 / 菜单），按主题 token 取色、按节点声明绑定事件。
//!
//! 状态与接线（视图 kind 编解码、贡献查询、输入草稿、面树投影、事件回发）在
//! [`crate::plugin_actions`]；本模块只读 `App` 里的投影缓存，不持运行时锁。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    column, container, grid, image, keyed_column, mouse_area, progress_bar, row, space,
    text_editor, text_input, Stack,
};
use iced::{Color, Element, Length, Padding};

use atelyx_core::plugin_runtime::{
    Align, Axis, ButtonVariant, CardTone, ColorToken, ControlSize, EventKind, EventValue, Fill,
    ImageFit, ImageSource, Justify, NodeBody, TextWeight, ViewEvent, ViewNode,
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

/// 面身份与所在窗口（弹层与图标钮悬停提示都要窗口 id）。
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
        Some(pad) => Padding::from(pad),
        None => Padding::ZERO.horizontal(12.0).vertical(10.0),
    };
    match app.plugin_trees.get(&key) {
        Some(tree) => container(render_root(app, &ctx, tree))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(outer_pad)
            .into(),
        None => placeholder(
            contribution.icon.as_deref().unwrap_or("puzzle"),
            &contribution.label,
            "等待插件提交界面（插件未运行或尚未就绪）",
        ),
    }
}

fn placeholder<'a>(icon_name: &str, title: &str, desc: &str) -> Element<'a, Message> {
    controls::view_placeholder(icon_name, title.to_string(), desc.to_string())
}

/// 根节点渲染：容器按面区域撑满高度（Fill 子项——画布 / 列表——才有可分配空间；
/// Shrink 根列内的 Fill 子项会塌缩成 0 高）。嵌套容器保持收缩语义。
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
    match &node.body {
        NodeBody::Column { gap, padding, cross_align } => {
            let mut col = keyed_column(children(app, ctx, node))
                .width(Length::Fill)
                .height(if fill_height { Length::Fill } else { Length::Shrink })
                .spacing(gap.unwrap_or(DEFAULT_GAP));
            if let Some(pad) = padding {
                col = col.padding(Padding::from(*pad));
            }
            if let Some(align) = cross_align {
                col = col.align_items(alignment(*align));
            }
            col.into()
        }
        NodeBody::Row { gap, padding, justify, cross_align, fill } => {
            let spacing = gap.unwrap_or(DEFAULT_GAP);
            let declared = justify.is_some();
            let justify = justify.unwrap_or(Justify::Start);
            let kids = children_elements(app, ctx, node);
            // spaceBetween：在占满宽度的行里插入弹性空档把子项推向两端
            let mut line = if justify == Justify::SpaceBetween {
                let mut els = Vec::new();
                for (index, child) in kids.into_iter().enumerate() {
                    if index > 0 {
                        els.push(space().width(Length::Fill).into());
                    }
                    els.push(child);
                }
                row(els).width(Length::Fill)
            } else {
                row(kids)
            }
            .spacing(spacing)
            .align_y(cross_align.map(vertical).unwrap_or(Vertical::Center));
            // fill 行：宽度 Fill 关掉子项的宽度压缩（Fill 子项均分剩余宽度）；
            // both 再加高度 Fill，让行延迟到父列的第三趟布局、占满剩余高度（分栏的前提）
            match fill {
                Some(Fill::Both) => {
                    line = line.width(Length::Fill).height(Length::Fill);
                }
                Some(Fill::Width) => {
                    line = line.width(Length::Fill);
                }
                None => {}
            }
            if fill_height {
                line = line.height(Length::Fill);
            }
            if let Some(pad) = padding {
                line = line.padding(Padding::from(*pad));
            }
            // 声明了 justify 才谈得上主轴分布（spaceBetween 行自身已撑满，其余套占满宽度的容器）；
            // 未声明时保持 row 原样——宽度收缩，随父列的 cross_align 对齐
            if !declared {
                line.into()
            } else if justify == Justify::SpaceBetween {
                line.into()
            } else {
                container(line)
                    .width(Length::Fill)
                    .align_x(justify_x(justify))
                    .into()
            }
        }
        NodeBody::Card { gap, padding, accent, tone, radius, bordered } => {
            let inner = keyed_column(children(app, ctx, node))
                .width(Length::Fill)
                .height(Length::Shrink)
                .spacing(gap.unwrap_or(DEFAULT_GAP));
            let style = card_style(accent.unwrap_or_default(), *tone, *radius, *bordered);
            let mut card = container(inner)
                .width(Length::Fill)
                .padding(Padding::from(padding.unwrap_or(10.0)))
                .style(move |_t| style);
            if fill_height {
                card = card.height(Length::Fill);
            }
            card.into()
        }
        NodeBody::Stack {} => {
            let mut stack = Stack::new();
            for child in children_elements(app, ctx, node) {
                stack = stack.push(child);
            }
            let element: Element<'a, Message> = stack.into();
            if fill_height {
                container(element).width(Length::Fill).height(Length::Fill).into()
            } else {
                element
            }
        }
        NodeBody::Scroll { direction, gap } => {
            let spacing = gap.unwrap_or(DEFAULT_GAP);
            let inner: Element<Message> = match direction.unwrap_or(Axis::Vertical) {
                Axis::Horizontal => row(children_elements(app, ctx, node)).spacing(spacing).into(),
                Axis::Vertical => column(children_elements(app, ctx, node)).spacing(spacing).into(),
            };
            scroll::v(inner)
                .id(node_scroll_id(ctx, &node.id))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        NodeBody::Grid { columns, gap } => grid(children_elements(app, ctx, node))
            .columns((*columns).max(1) as usize)
            .spacing(gap.unwrap_or(DEFAULT_GAP))
            .into(),
        NodeBody::List { gap } => scroll::v(
            keyed_column(children(app, ctx, node))
                .width(Length::Fill)
                .height(Length::Shrink)
                .spacing(gap.unwrap_or(DEFAULT_GAP)),
        )
        .id(node_scroll_id(ctx, &node.id))
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        NodeBody::Divider {} => controls::h_divider_subtle(),
        NodeBody::Text { content, size, weight, color, align } => {
            let line = text(content.as_str())
                .size(pal::fs_body() * size.unwrap_or(1.0))
                .font(weight.map(text_weight).unwrap_or_default())
                .color(token_color(color.unwrap_or(ColorToken::Text)));
            container(line)
                .width(Length::Fill)
                .align_x(horizontal(align.unwrap_or(Align::Start)))
                .into()
        }
        NodeBody::Markdown { size, .. } => {
            let key = (ctx.plugin_id.clone(), ctx.surface_id.clone(), node.id.clone());
            match app.plugin_markdown.get(&key) {
                Some(entry) => crate::markdown_view::document_body_scaled(
                    &entry.doc,
                    &app.theme,
                    &app.note_images,
                    app.global.soft_wrap,
                    size.unwrap_or(1.0),
                ),
                None => text("").into(),
            }
        }
        NodeBody::TextInput { placeholder, multiline, password, .. } => {
            if multiline.unwrap_or(false) {
                multiline_input(app, ctx, node, placeholder.as_deref().unwrap_or_default())
            } else {
                single_line_input(
                    app,
                    ctx,
                    node,
                    placeholder.as_deref().unwrap_or_default(),
                    password.unwrap_or(false),
                )
            }
        }
        NodeBody::NumberInput { placeholder, .. } => single_line_input(
            app,
            ctx,
            node,
            placeholder.as_deref().unwrap_or_default(),
            false,
        ),
        NodeBody::Dropdown { options, selected, placeholder } => {
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
            let open = declares(node, EventKind::Change).then(|| Message::PluginList {
                window: ctx.window,
                entries,
            });
            controls::dropdown_trigger(&label, Length::Fill, open)
        }
        NodeBody::Checkbox { checked, label } => {
            let state = if *checked { controls::CheckState::On } else { controls::CheckState::Off };
            let on_press = declares(node, EventKind::Change).then(|| {
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
        NodeBody::Button { label, variant, enabled, size } => controls::btn(
            label.as_str(),
            &button_size(*size),
            button_variant(*variant),
            enabled.unwrap_or(true).then(|| click(ctx, node)).flatten(),
        )
        .into(),
        NodeBody::IconButton { icon, tooltip, enabled, size, variant, hover_only } => {
            let (icon_px, box_px) = icon_box(*size);
            let variant = icon_variant(*variant);
            let on_press = enabled.unwrap_or(true).then(|| click(ctx, node)).flatten();
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
        NodeBody::Image { source, fit, width, height } => {
            let el = image_node(app, &ctx.plugin_id, source, fit.unwrap_or(ImageFit::Contain), *width, *height);
            match click(ctx, node) {
                Some(msg) => mouse_area(el).on_press(msg).into(),
                None => el,
            }
        }
        NodeBody::Progress { value } => progress_bar(0.0..=1.0, value.clamp(0.0, 1.0) as f32)
            .length(Length::Fill)
            .into(),
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
fn children<'a>(app: &'a App, ctx: &Ctx, node: &'a ViewNode) -> Vec<(u64, Element<'a, Message>)> {
    node.children
        .iter()
        .map(|child| (node_key(&child.id), render_node(app, ctx, child)))
        .collect()
}

/// 子节点元素序列（行 / 滚动 / 网格容器用）。
fn children_elements<'a>(app: &'a App, ctx: &Ctx, node: &'a ViewNode) -> Vec<Element<'a, Message>> {
    node.children.iter().map(|child| render_node(app, ctx, child)).collect()
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
            .style(|_t| container::Style {
                background: Some(pal::BG_TERTIARY.into()),
                border: iced::border::rounded(pal::RADIUS_SM).color(pal::BORDER_STRONG).width(1.0),
                ..container::Style::default()
            })
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
/// `width` / `height` 给定时按该尺寸渲染（缺省按原图尺寸）。
fn image_node<'a>(
    app: &'a App,
    plugin_id: &str,
    source: &ImageSource,
    fit: ImageFit,
    width: Option<f32>,
    height: Option<f32>,
) -> Element<'a, Message> {
    let handle = image_key(app, plugin_id, source).and_then(|key| app.note_images.get(&key).cloned());
    match handle {
        Some(handle) => {
            let mut widget = image(handle).content_fit(content_fit(fit));
            if let Some(width) = width {
                widget = widget.width(Length::Fixed(width));
            }
            if let Some(height) = height {
                widget = widget.height(Length::Fixed(height));
            }
            widget.into()
        }
        None => placeholder("image", "图片", "加载中或不可用"),
    }
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

/// 某节点的悬停态（`hoverOnly` 图标钮据此显示 / 隐藏）。
fn hovered(app: &App, ctx: &Ctx, node_id: &str) -> bool {
    app.plugin_hovered.contains(&(ctx.plugin_id.clone(), ctx.surface_id.clone(), node_id.to_string()))
}

/// 悬停进出消息（`hoverOnly` 图标钮的显隐开关）。
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

fn token_color(token: ColorToken) -> Color {
    crate::plugin_canvas::token_color(token)
}

fn text_weight(weight: TextWeight) -> iced::Font {
    crate::plugin_canvas::font_of(weight)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_keys_are_stable_and_distinct() {
        assert_eq!(node_key("status"), node_key("status"), "同一节点 id 的键必须稳定");
        assert_ne!(node_key("status"), node_key("status2"));
    }
}
