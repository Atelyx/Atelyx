//! 插件面板渲染：把 [`ViewNode`] 树解释成 iced 控件（布局容器 / 文本 / 输入 / 按钮 / 列表 / 图片 /
//! 进度条 / 菜单），按主题 token 取色、按节点声明绑定事件。
//!
//! 状态与接线（视图 kind 编解码、贡献查询、输入草稿、面树投影、事件回发）在
//! [`crate::plugin_actions`]；本模块只读 `App` 里的投影缓存，不持运行时锁。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    column, container, grid, image, keyed_column, progress_bar, row, scrollable, text, text_editor,
    text_input,
};
use iced::{Color, Element, Length, Padding};

use atelyx_core::plugin_runtime::{
    Align, Axis, ButtonVariant, ColorToken, EventKind, EventValue, ImageFit, ImageSource, NodeBody,
    TextWeight, ViewEvent, ViewNode,
};

use crate::plugin_actions::{contribution, declares, image_key, input_display, node_value_text, PopupEntry};
use crate::ui::controls::{self, Variant};
use crate::ui::palette as pal;
use crate::{App, Message};

/// 容器缺省间距（插件未声明 gap 时）。
const DEFAULT_GAP: f32 = 6.0;
/// 单行输入的内边距（与设置表单同口径）。
const INPUT_PAD_X: f32 = 8.0;
const INPUT_PAD_Y: f32 = 6.0;
/// 多行输入高度。
const EDITOR_H: f32 = 96.0;

/// 面身份与所在窗口（弹层与图标钮悬停提示都要窗口 id）。
struct Ctx {
    plugin_id: String,
    surface_id: String,
    window: iced::window::Id,
}

/// 插件面板内容：取投影缓存里的树渲染；贡献缺失 / 树未提交时给可读占位。
pub fn panel_view<'a>(app: &'a App, window: iced::window::Id, view: &str) -> Element<'a, Message> {
    let Some((plugin_id, surface_id)) = crate::plugin_actions::parse_view_kind(view) else {
        return placeholder("puzzle", "插件面板", "视图 kind 非法");
    };
    let Some(contribution) = contribution(app, plugin_id, surface_id) else {
        return placeholder(
            "puzzle",
            &format!("插件面板 {plugin_id}"),
            "插件不可用：未安装、未启用或已卸载",
        );
    };
    let ctx = Ctx {
        plugin_id: plugin_id.to_string(),
        surface_id: surface_id.to_string(),
        window,
    };
    let key = (ctx.plugin_id.clone(), ctx.surface_id.clone());
    match app.plugin_trees.get(&key) {
        Some(tree) => container(render_root(app, &ctx, tree))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::ZERO.horizontal(12.0).vertical(10.0))
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
        NodeBody::Column { .. } | NodeBody::Row { .. } => render_node_in(app, ctx, node, true),
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
        NodeBody::Row { gap, padding, align, cross_align } => {
            let mut line = row(children_elements(app, ctx, node))
                .spacing(gap.unwrap_or(DEFAULT_GAP))
                .align_y(cross_align.map(vertical).unwrap_or(Vertical::Center));
            if fill_height {
                line = line.height(Length::Fill);
            }
            if let Some(pad) = padding {
                line = line.padding(Padding::from(*pad));
            }
            match align {
                // 主轴对齐要在行占满宽度后才谈得上分布（不填充时子项紧贴左缘，对齐无意义）
                Some(align) => container(line)
                    .width(Length::Fill)
                    .align_x(horizontal(*align))
                    .into(),
                None => line.into(),
            }
        }
        NodeBody::Scroll { direction } => {
            let inner: Element<Message> = match direction.unwrap_or(Axis::Vertical) {
                Axis::Horizontal => row(children_elements(app, ctx, node)).spacing(DEFAULT_GAP).into(),
                Axis::Vertical => column(children_elements(app, ctx, node)).spacing(DEFAULT_GAP).into(),
            };
            scrollable(inner).width(Length::Fill).height(Length::Fill).into()
        }
        NodeBody::Grid { columns, gap } => grid(children_elements(app, ctx, node))
            .columns((*columns).max(1) as usize)
            .spacing(gap.unwrap_or(DEFAULT_GAP))
            .into(),
        NodeBody::List {} => scrollable(
            keyed_column(children(app, ctx, node))
                .width(Length::Fill)
                .height(Length::Shrink)
                .spacing(DEFAULT_GAP),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        NodeBody::Text { content, size, weight, color } => text(content.as_str())
            .size(size.unwrap_or_else(pal::fs_body))
            .font(weight.map(text_weight).unwrap_or_default())
            .color(token_color(color.unwrap_or(ColorToken::Text)))
            .width(Length::Fill)
            .into(),
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
        NodeBody::Button { label, variant, enabled } => controls::btn(
            label.as_str(),
            &controls::btn_sm(),
            button_variant(*variant),
            enabled.unwrap_or(true).then(|| click(ctx, node)).flatten(),
        )
        .into(),
        NodeBody::IconButton { icon, tooltip, enabled } => controls::icon_button(
            icon,
            14.0,
            controls::ICON_MD,
            Variant::Ghost,
            enabled.unwrap_or(true).then(|| click(ctx, node)).flatten(),
            tooltip.as_ref().map(|tip| (ctx.window, tip.clone())),
        ),
        NodeBody::Image { source, fit } => {
            image_node(app, &ctx.plugin_id, source, fit.unwrap_or(ImageFit::Contain))
        }
        NodeBody::Progress { value } => progress_bar(0.0..=1.0, value.clamp(0.0, 1.0) as f32)
            .length(Length::Fill)
            .into(),
        NodeBody::Menu { items, label, icon } => {
            let entries = items
                .iter()
                .map(|item| PopupEntry {
                    label: item.label.clone(),
                    current: false,
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
                (Some(label), _) => controls::dropdown_trigger(label, Length::Fill, open),
                (None, Some(icon)) => {
                    controls::icon_button(icon, 14.0, controls::ICON_MD, Variant::Ghost, open, None)
                }
                (None, None) => controls::dropdown_trigger("…", Length::Fill, open),
            }
        }
        NodeBody::Canvas { mouse_events } => {
            crate::plugin_canvas::PluginCanvas {
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
            .into()
        }
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
fn image_node<'a>(app: &'a App, plugin_id: &str, source: &ImageSource, fit: ImageFit) -> Element<'a, Message> {
    let handle = image_key(app, plugin_id, source).and_then(|key| app.note_images.get(&key).cloned());
    match handle {
        Some(handle) => image(handle).content_fit(content_fit(fit)).into(),
        None => placeholder("image", "图片", "加载中或不可用"),
    }
}

fn token_color(token: ColorToken) -> Color {
    crate::plugin_canvas::token_color(token)
}

fn text_weight(weight: TextWeight) -> iced::Font {
    crate::plugin_canvas::font_of(weight)
}

fn button_variant(variant: Option<ButtonVariant>) -> Variant {
    match variant {
        None => Variant::Secondary,
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
