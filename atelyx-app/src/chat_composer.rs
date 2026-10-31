//! 对话输入区共享组装：面板与画布对话卡同一套输入渲染与动作路由，差异只由作用域决定
//! （草稿 / 引用 / 附件归作用域；Agent 与模型配置按写入目标落到面板覆盖或卡节点配置）。

use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, operation, row, scrollable, Space};
use iced::{border, Length, Padding, Task};
use serde_json::Value;

use crate::canvas_view::CanvasMsg;
use crate::chat_actions::{ChatMsg, MentionPicker, MentionTarget, PendingImage};
use crate::ui::icon::icon;
use crate::ui::{controls, palette as pal, scroll, text};
use crate::{App, Message};

/// 候选行尺寸（缩放前；与搜索结果行同口径）。
const PICKER_ROW_PAD_X: f32 = 10.0;
const PICKER_ROW_PAD_Y: f32 = 5.0;
const PICKER_ROW_LINE_GAP: f32 = 1.0;
const PICKER_ICON: f32 = 12.0;
const PICKER_ROW_GAP: f32 = 6.0;
/// 浮层列表最大高度（缩放前）。
const PICKER_MAX_H: f32 = 224.0;
/// 文字行高倍率（iced 文本默认 `LineHeight::Relative(1.3)`，估算行高随之）。
const PICKER_LINE_RATIO: f32 = 1.3;
/// 画布卡候选的次行标注（卡片没有所在文件夹）。
const CARD_SUB_LABEL: &str = "画布卡片";

/// 输入区作用域：决定草稿 / 引用 / 附件托盘与消息动作落在谁身上。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerScope {
    /// 面板草稿（普通会话与面板绑定画布卡会话共用；后者的配置目标随会话指向卡）。
    Panel,
    /// 画布卡内输入：草稿归卡运行态，配置目标即本卡。
    Card { node: String },
}

/// Agent / 模型配置写入目标（与草稿作用域正交：面板绑定卡会话 = 面板草稿 + 卡配置）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigScope {
    /// 写面板级覆盖（模型 / 等级随面板持久化，Agent 写会话 agentId）。
    Panel,
    /// 写卡节点配置（`data.agentId` / `providerId` / `model` / `reasoningEffort`，随 `.atlx` 持久化）。
    Card { node: String },
}

/// 面板作用域的配置目标：绑定画布卡会话时写卡节点配置，其余写面板覆盖。
fn config_scope(app: &App, scope: &ComposerScope) -> ConfigScope {
    match scope {
        ComposerScope::Card { node } => ConfigScope::Card { node: node.clone() },
        ComposerScope::Panel => {
            match app.chat.active.as_deref().and_then(crate::canvas_chat::node_of_session) {
                Some(node) => ConfigScope::Card { node: node.to_string() },
                None => ConfigScope::Panel,
            }
        }
    }
}

/// 配置目标卡是否被对端独占编辑（配置属卡内容，锁作用范围内触发钮禁用）。
fn config_locked(app: &App, config: &ConfigScope) -> bool {
    match config {
        ConfigScope::Panel => false,
        ConfigScope::Card { node } => matches!(
            crate::space_actions::conversation_lock_holder(app, node),
            crate::space_actions::LockHolder::Peer { .. }
        ),
    }
}

/// 输入区整体（# 候选浮层 + 附件托盘 + 输入盒：输入面 + 工具排）。
/// 卡作用域下卡运行态缺失返回 None（节点集合与运行态由 `sync_card_chats` 对齐）。
pub fn composer<'a>(
    app: &'a App,
    window: iced::window::Id,
    scope: &ComposerScope,
    scale: f32,
) -> Option<iced::Element<'a, Message>> {
    let (draft, refs, picker, images, route) = match scope {
        ComposerScope::Panel => (
            &app.chat.input,
            &app.chat.mention_refs,
            &app.chat.mention_picker,
            &app.chat.pending_images,
            crate::chat_input::InputRoute::Panel,
        ),
        ComposerScope::Card { node } => {
            let chat = app.canvas.as_ref()?.view.card_chat.get(node)?;
            (
                &chat.draft,
                &chat.mention_refs,
                &chat.mention_picker,
                &chat.pending_images,
                crate::chat_input::InputRoute::Card(node.clone()),
            )
        }
    };
    let streaming = match scope {
        ComposerScope::Card { node } => crate::canvas_chat::is_streaming(app, node),
        ComposerScope::Panel => crate::chat_view::is_streaming(app),
    };
    let s = scale;
    let mut inner = column![].spacing(0).width(Length::Fill);
    if let Some(picker) = picker {
        inner = inner.push(mention_picker_view(app, picker, scope, s));
    }
    // 自绘输入面（# 标签高亮 / 整标签交互由部件托管）
    let surface = crate::chat_input::Input::new(draft, refs, route)
        .picker_open(picker.is_some())
        .placeholder("输入消息，Enter 发送，Shift+Enter 换行")
        .scale(s);
    inner = inner.push(
        container(iced::Element::new(surface))
            .width(Length::Fill)
            .padding(Padding::ZERO.left(2.0 * s).right(2.0 * s).top(8.0 * s)),
    );
    inner = inner.push(
        container(tool_row(app, window, scope, streaming, s))
            .width(Length::Fill)
            .padding(Padding::ZERO.top(4.0 * s)),
    );
    let input_box = container(inner)
        .width(Length::Fill)
        .padding(Padding::new(8.0 * s))
        .style(|_| container::Style {
            background: Some(pal::BG_SUNKEN.into()),
            border: border::Border {
                width: 0.5,
                radius: pal::RADIUS_MD.into(),
                color: pal::BORDER,
            },
            ..Default::default()
        });
    let mut col = column![].width(Length::Fill);
    if !images.is_empty() {
        col = col.push(image_tray(scope, images, s));
    }
    col = col.push(input_box);
    Some(col.into())
}

/// 工具排（输入盒内底部）：左 + / Agent / 模型·等级；右 发送 / 停止。
fn tool_row<'a>(
    app: &'a App,
    window: iced::window::Id,
    scope: &ComposerScope,
    streaming: bool,
    s: f32,
) -> iced::Element<'a, Message> {
    let config = config_scope(app, scope);
    let plus_open = app
        .popup
        .as_ref()
        .is_some_and(|(_, open)| matches!(&open.state, crate::layout_view::PopupState::ChatPlusMenu { scope: open } if *open == *scope));
    let plus_btn = button(icon(
        "plus",
        (13.0 * s).max(8.0),
        if plus_open { pal::ACCENT } else { pal::TEXT_SECONDARY },
    ))
    .padding(Padding::new(6.0 * s).vertical(4.0 * s))
    .style(controls::row_style(false, pal::RADIUS_XS))
    .on_press(crate::layout_view::btn_popup_min(
        window,
        app.cursor_for(window),
        crate::layout_view::PopupState::ChatPlusMenu { scope: scope.clone() },
        0.0,
    ));
    let send_btn = if streaming {
        controls::icon_button(
            "square",
            (14.0 * s).max(9.0),
            (28.0 * s).max(18.0),
            controls::Variant::Secondary,
            Some(send_stop_msg(scope, true)),
            Some((window, "停止生成".to_string())),
        )
    } else {
        controls::icon_button(
            "arrow-up",
            (14.0 * s).max(9.0),
            (28.0 * s).max(18.0),
            controls::Variant::Primary,
            Some(send_stop_msg(scope, false)),
            Some((window, "发送（Enter）".to_string())),
        )
    };
    row![
        plus_btn,
        agent_trigger(app, window, &config, s),
        model_trigger(app, window, &config, s),
        Space::new().width(Length::Fill),
        send_btn,
    ]
    .spacing(6.0 * s)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

fn send_stop_msg(scope: &ComposerScope, stop: bool) -> Message {
    match scope {
        ComposerScope::Panel => {
            if stop {
                Message::Chat(ChatMsg::Stop)
            } else {
                Message::Chat(ChatMsg::Send)
            }
        }
        ComposerScope::Card { node } => {
            let msg = if stop {
                crate::canvas_chat::CanvasChatMsg::Stop(node.clone())
            } else {
                crate::canvas_chat::CanvasChatMsg::Send(node.clone())
            };
            Message::Canvas(CanvasMsg::Chat(msg))
        }
    }
}

/// Agent 配置当前显示名（缺省 = 内置「对话」）。
fn agent_label(app: &App, config: &ConfigScope) -> String {
    let id = match config {
        ConfigScope::Panel => app.chat.active_agent_id(),
        ConfigScope::Card { node } => app
            .canvas
            .as_ref()
            .and_then(|s| s.data.nodes.iter().find(|n| &n.id == node))
            .and_then(|n| n.data.get("agentId").and_then(Value::as_str).map(str::to_string)),
    };
    match id {
        Some(id) => app
            .agents
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.name.clone())
            .unwrap_or(id),
        None => "对话".to_string(),
    }
}

fn agent_trigger<'a>(
    app: &'a App,
    window: iced::window::Id,
    config: &ConfigScope,
    s: f32,
) -> iced::Element<'a, Message> {
    let locked = config_locked(app, config);
    button(
        row![
            icon("bot", (12.0 * s).clamp(8.0, 14.0), pal::TEXT_SECONDARY),
            text(agent_label(app, config))
                .size((pal::fs_micro() * s).max(8.0))
                .color(pal::TEXT_SECONDARY),
            icon("chevron-down", (10.0 * s).clamp(7.0, 12.0), pal::TEXT_MUTED),
        ]
        .spacing((4.0 * s).max(1.0))
        .align_y(iced::alignment::Vertical::Center),
    )
    .padding(Padding::new(6.0 * s).vertical(3.0 * s))
    .style(controls::row_style(false, pal::RADIUS_XS))
    .on_press_maybe((!locked).then(|| {
        crate::layout_view::btn_popup_min(
            window,
            app.cursor_for(window),
            crate::layout_view::PopupState::ChatAgentMenu { scope: config.clone() },
            0.0,
        )
    }))
    .into()
}

/// 模型/推理等级复合触发钮：显示「模型 · 等级」，两级菜单分别设置。
fn model_trigger<'a>(
    app: &'a App,
    window: iced::window::Id,
    config: &ConfigScope,
    s: f32,
) -> iced::Element<'a, Message> {
    let model = match config {
        ConfigScope::Panel => crate::layout_view::chat_model_trigger_label(app),
        ConfigScope::Card { node } => crate::layout_view::canvas_card_model_label(app, node),
    };
    let effort = match config {
        ConfigScope::Panel => crate::layout_view::chat_effort_label(app),
        ConfigScope::Card { node } => crate::layout_view::canvas_card_effort_label(app, node),
    };
    let locked = config_locked(app, config);
    button(
        row![
            icon("sparkles", (12.0 * s).clamp(8.0, 14.0), pal::TEXT_SECONDARY),
            text(format!("{model} · {effort}"))
                .size((pal::fs_micro() * s).max(8.0))
                .color(pal::TEXT_SECONDARY),
            icon("chevron-down", (10.0 * s).clamp(7.0, 12.0), pal::TEXT_MUTED),
        ]
        .spacing((4.0 * s).max(1.0))
        .align_y(iced::alignment::Vertical::Center),
    )
    .padding(Padding::new(6.0 * s).vertical(3.0 * s))
    .style(controls::row_style(false, pal::RADIUS_XS))
    .on_press_maybe((!locked).then(|| {
        crate::layout_view::btn_popup_min(
            window,
            app.cursor_for(window),
            crate::layout_view::PopupState::ChatModelMenu { scope: config.clone() },
            0.0,
        )
    }))
    .into()
}

/// # 引用候选浮层（输入面上方；画布卡目标与仓库文件混排，字号随缩放）。
/// 条目为双行卡片：主行文件名（按可用宽省略，不换行）+ 次行所在文件夹；
/// 键盘选中的项用强调底色，其余项悬停时出浅底反馈。
fn mention_picker_view<'a>(
    app: &'a App,
    picker: &'a MentionPicker,
    scope: &ComposerScope,
    s: f32,
) -> iced::Element<'a, Message> {
    // 候选行的点击消息只差作用域来源：卡作用域把节点 id 取出来，浮层闭包不持有 scope 借用
    let card_node = match scope {
        ComposerScope::Panel => None,
        ComposerScope::Card { node } => Some(node.clone()),
    };
    let pad_x = PICKER_ROW_PAD_X * s;
    let pad_y = (PICKER_ROW_PAD_Y * s).max(2.0);
    let icon_px = (PICKER_ICON * s).clamp(9.0, 14.0);
    let gap = (PICKER_ROW_GAP * s).max(2.0);
    let indent = icon_px + gap;
    let fs_name = (pal::fs_caption() * s).max(9.0);
    let fs_sub = (pal::fs_micro() * s).max(8.0);
    let list = iced::widget::responsive(move |size| {
        // 主行可用宽 = 浮层宽 − 行内衬 − 图标列；次行再缩进一个图标列
        let name_w = (size.width - pad_x * 2.0 - indent).max(0.0);
        let sub_w = (name_w - indent).max(0.0);
        let rows = picker.candidates.iter().enumerate().map(|(i, target)| {
            let selected = i == picker.selected;
            let (icon_name, name) = match target {
                MentionTarget::Vault { rel, is_dir } => {
                    let icon_name = if *is_dir {
                        "folder"
                    } else {
                        match atelyx_core::ai::refs::kind_of_file(rel) {
                            Some(atelyx_core::ai::refs::RefKind::Table) => "table",
                            Some(atelyx_core::ai::refs::RefKind::Canvas) => "layers",
                            _ => "file-text",
                        }
                    };
                    let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
                    (icon_name, name)
                }
                MentionTarget::Card { node_id } => {
                    let title = app
                        .canvas
                        .as_ref()
                        .and_then(|cvs| cvs.data.nodes.iter().find(|n| &n.id == node_id))
                        .map(crate::canvas_chat::card_title)
                        .unwrap_or_else(|| node_id.clone());
                    ("box", title)
                }
            };
            let press = match &card_node {
                None => Message::Chat(ChatMsg::MentionPick(i)),
                Some(node) => Message::Canvas(CanvasMsg::Chat(
                    crate::canvas_chat::CanvasChatMsg::MentionPick { node: node.clone(), index: i },
                )),
            };
            let mut lines = column![
                row![
                    icon(
                        icon_name,
                        icon_px,
                        if selected { pal::ACCENT } else { pal::TEXT_SECONDARY },
                    ),
                    text(crate::layout_view::ellipsize(&name, name_w, fs_name))
                        .size(fs_name)
                        .wrapping(Wrapping::None)
                        .color(if selected { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY }),
                ]
                .spacing(gap)
                .align_y(iced::alignment::Vertical::Center),
            ]
            .spacing((PICKER_ROW_LINE_GAP * s).max(1.0));
            if let Some(sub) = picker_row_sub(target) {
                lines = lines.push(
                    container(
                        text(crate::layout_view::ellipsize(sub, sub_w, fs_sub))
                            .size(fs_sub)
                            .wrapping(Wrapping::None)
                            .color(pal::TEXT_MUTED),
                    )
                    .padding(Padding::ZERO.left(indent)),
                );
            }
            button(container(lines).width(Length::Fill))
                .width(Length::Fill)
                .padding(Padding::ZERO.horizontal(pad_x).vertical(pad_y))
                .style(controls::row_style(selected, pal::RADIUS_SM))
                .on_press(press)
        });
        column(rows.map(iced::Element::from)).spacing(0).width(Length::Fill).into()
    })
    .height(Length::Shrink);
    let list = container(
        scroll::v(list).id(mention_scroll_id(scope)).height(Length::Shrink).width(Length::Fill),
    )
    .max_height(PICKER_MAX_H * s);
    container(list)
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(pal::BG_SECONDARY.into()),
            border: border::Border {
                width: 0.5,
                radius: pal::RADIUS_MD.into(),
                color: pal::BORDER,
            },
            ..Default::default()
        })
        .into()
}

/// 候选浮层滚动区 id：面板与任一画布卡各一份（同屏并存时不互相定位）。
pub(crate) fn mention_scroll_id(scope: &ComposerScope) -> iced::widget::Id {
    match scope {
        ComposerScope::Panel => iced::widget::Id::new("chat-mention"),
        ComposerScope::Card { node } => {
            iced::widget::Id::from(format!("chat-mention-card:{node}"))
        }
    }
}

/// 选中项跟滚：把选中行带回可视区（未溢出、选中行本就在可视区内时不移动）。
pub(crate) fn mention_follow(
    scope: &ComposerScope,
    picker: &MentionPicker,
    scale: f32,
) -> Task<Message> {
    let heights: Vec<f32> = picker.candidates.iter().map(|t| picker_row_h(t, scale)).collect();
    let y = follow_ratio(&heights, picker.selected, PICKER_MAX_H * scale);
    operation::snap_to(mention_scroll_id(scope), scrollable::RelativeOffset { x: 0.0, y })
}

/// 候选行高（与浮层行排布同源：内衬 + 文字行高，次行另加行距）。
fn picker_row_h(target: &MentionTarget, scale: f32) -> f32 {
    let pad_y = (PICKER_ROW_PAD_Y * scale).max(2.0);
    let mut h = pad_y * 2.0 + (pal::fs_caption() * scale).max(9.0) * PICKER_LINE_RATIO;
    if picker_row_sub(target).is_some() {
        let gap = (PICKER_ROW_LINE_GAP * scale).max(1.0);
        h += gap + (pal::fs_micro() * scale).max(8.0) * PICKER_LINE_RATIO;
    }
    h
}

/// 候选的次行文本：仓库文件取所在文件夹（仓库根下的文件没有次行），画布卡给固定标注。
fn picker_row_sub(target: &MentionTarget) -> Option<&str> {
    match target {
        MentionTarget::Vault { rel, .. } => rel.rsplit_once('/').map(|(dir, _)| dir),
        MentionTarget::Card { .. } => Some(CARD_SUB_LABEL),
    }
}

/// 选中行跟滚的相对位置：滚动量取「选中行底缘贴到可视区底缘」，未越过时不滚。
/// 行高逐行累加（候选分单行 / 双行，不能按等行高折算）。
fn follow_ratio(heights: &[f32], selected: usize, viewport: f32) -> f32 {
    let total: f32 = heights.iter().sum();
    let range = total - viewport;
    if heights.is_empty() || range <= 0.0 {
        return 0.0;
    }
    let selected = selected.min(heights.len() - 1);
    let bottom: f32 = heights[..=selected].iter().sum();
    ((bottom - viewport).max(0.0) / range).clamp(0.0, 1.0)
}

/// 待发送图片托盘：输入盒上方一行缩略图 chip（点击移除）。
fn image_tray<'a>(
    scope: &ComposerScope,
    images: &'a [PendingImage],
    s: f32,
) -> iced::Element<'a, Message> {
    let chips = images.iter().enumerate().map(|(i, img)| {
        let remove = match scope {
            ComposerScope::Panel => Message::Chat(ChatMsg::RemoveAttachment(i)),
            ComposerScope::Card { node } => Message::Canvas(CanvasMsg::Chat(
                crate::canvas_chat::CanvasChatMsg::RemoveAttachment { node: node.clone(), index: i },
            )),
        };
        let name = img.file_name.clone();
        button(
            row![
                icon("image", (12.0 * s).clamp(9.0, 14.0), pal::TEXT_SECONDARY),
                text(name).size((pal::fs_micro() * s).max(8.0)).color(pal::TEXT_SECONDARY),
                icon("x", (10.0 * s).clamp(7.0, 12.0), pal::TEXT_MUTED),
            ]
            .spacing((4.0 * s).max(1.0))
            .align_y(iced::alignment::Vertical::Center),
        )
        .padding(Padding::new(8.0 * s).vertical(3.0 * s))
        .style(controls::row_style(false, pal::RADIUS_XS))
        .on_press(remove)
    });
    container(scroll::v(row(chips.map(iced::Element::from)).spacing(6.0 * s)))
        .width(Length::Fill)
        .padding(Padding::new(12.0 * s).top(8.0 * s).bottom(0.0))
        .into()
}

/// 对话卡头控制段（流式标注 + 压缩）：浮层内容锚在卡头右段，
/// 标题省略宽度由画布侧 `canvas_view::header_ctrl_reserve` 预留同一定额。
/// Agent 与模型选择在输入区工具排（面板与卡同构），不重复放卡头。
/// 被对端独占编辑时压缩禁用（配置属卡内容，锁作用范围内不写）。
pub fn card_header_controls<'a>(
    app: &'a App,
    window: iced::window::Id,
    node: &'a atelyx_core::vault::CanvasFileNode,
    zoom: f32,
) -> iced::Element<'a, Message> {
    use crate::canvas_chat::CanvasChatMsg;
    let s = zoom;
    let node_id = &node.id;
    let locked = matches!(
        crate::space_actions::conversation_lock_holder(app, node_id),
        crate::space_actions::LockHolder::Peer { .. }
    );
    let streaming = crate::canvas_chat::is_streaming(app, node_id);
    let compacting = app
        .canvas
        .as_ref()
        .and_then(|cvs| cvs.view.card_chat.get(node_id))
        .is_some_and(|c| c.compacting);
    let mut row = row![].spacing((4.0 * s).max(1.0)).align_y(iced::alignment::Vertical::Center);
    row = row.push(Space::new().width(Length::Fill));
    // 流式占位槽：固定宽度让压缩钮不随流式起止横移
    let slot = (12.0 * s).clamp(8.0, 14.0);
    row = row.push(if streaming {
        iced::Element::from(icon("loader-circle", slot, pal::ACCENT))
    } else {
        Space::new().width(Length::Fixed(slot)).into()
    });
    row = row.push(if compacting {
        controls::icon_button(
            "loader-circle",
            (13.0 * s).clamp(8.0, 14.0),
            (24.0 * s).clamp(16.0, 26.0),
            controls::Variant::Ghost,
            None,
            None,
        )
    } else {
        controls::icon_button(
            "layers",
            (13.0 * s).clamp(8.0, 14.0),
            (24.0 * s).clamp(16.0, 26.0),
            controls::Variant::Ghost,
            (!streaming && !locked)
                .then(|| Message::Canvas(CanvasMsg::Chat(CanvasChatMsg::Compact(node_id.clone())))),
            Some((window, "压缩会话历史（折叠为摘要，原文保留）".to_string())),
        )
    });
    container(row)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(iced::alignment::Vertical::Center)
        .padding(Padding::ZERO.right((8.0 * s).max(2.0)))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 选中行在首屏内不滚、越过底缘后按行贴底、末条滚到最底。
    #[test]
    fn follow_ratio_keeps_selection_visible() {
        let rows = [20.0f32; 10];
        assert_eq!(follow_ratio(&rows, 0, 60.0), 0.0);
        assert_eq!(follow_ratio(&rows, 2, 60.0), 0.0, "选中行仍整行在可视区内");
        // 第 4 行底缘越界 = 只滚一行，不是整屏
        let ratio = follow_ratio(&rows, 3, 60.0);
        assert!((ratio - 20.0 / 140.0).abs() < 1e-6, "越界一行滚一行：{ratio}");
        assert_eq!(follow_ratio(&rows, 9, 60.0), 1.0, "末条滚到底");
        assert_eq!(follow_ratio(&rows, 5, 400.0), 0.0, "未溢出不滚");
        assert_eq!(follow_ratio(&[], 0, 60.0), 0.0);
    }

    /// 单行与双行混排时按各自行高累加，不能按等行高折算。
    #[test]
    fn follow_ratio_accumulates_mixed_row_heights() {
        let rows = [26.0, 42.0, 26.0, 42.0];
        // 可视 68 = 26 + 42：选中第 2 行恰好贴底，不必滚
        assert_eq!(follow_ratio(&rows, 1, 68.0), 0.0);
        // 末行底缘 136，可视 68 → 滚到底
        assert_eq!(follow_ratio(&rows, 3, 68.0), 1.0);
    }

    /// 次行只在有内容时出现：仓库根下的文件单行，卡片恒双行。
    #[test]
    fn sub_line_only_for_nested_files_and_cards() {
        let file = |rel: &str| MentionTarget::Vault { rel: rel.into(), is_dir: false };
        assert_eq!(picker_row_sub(&file("甲.md")), None);
        assert_eq!(picker_row_sub(&file("笔记/乙.md")), Some("笔记"));
        assert_eq!(
            picker_row_sub(&MentionTarget::Card { node_id: "n1".into() }),
            Some(CARD_SUB_LABEL)
        );
        assert!(picker_row_h(&file("笔记/乙.md"), 1.0) > picker_row_h(&file("甲.md"), 1.0));
    }
}
