//! 对话输入区共享组装：面板与画布对话卡同一套输入渲染与动作路由，差异只由作用域决定
//! （草稿 / 引用 / 附件归作用域；Agent 与模型配置按写入目标落到面板覆盖或卡节点配置）。

use iced::widget::{button, column, container, row, Space};
use iced::{border, Length, Padding};
use serde_json::Value;

use crate::canvas_view::CanvasMsg;
use crate::chat_actions::{ChatMsg, MentionPicker, MentionTarget, PendingImage};
use crate::ui::icon::icon;
use crate::ui::{controls, palette as pal, scroll, text};
use crate::{App, Message};

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
fn mention_picker_view<'a>(
    app: &'a App,
    picker: &'a MentionPicker,
    scope: &ComposerScope,
    s: f32,
) -> iced::Element<'a, Message> {
    let rows = picker.candidates.iter().enumerate().map(|(i, target)| {
        let selected = i == picker.selected;
        let (icon_name, label, dir) = match target {
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
                let label = rel.rsplit('/').next().unwrap_or(rel).to_string();
                let dir = rel.rsplit_once('/').map(|(d, _)| d.to_string());
                (icon_name, label, dir)
            }
            MentionTarget::Card { node_id } => {
                let title = app
                    .canvas
                    .as_ref()
                    .and_then(|cvs| cvs.data.nodes.iter().find(|n| &n.id == node_id))
                    .map(crate::canvas_chat::card_title)
                    .unwrap_or_else(|| node_id.clone());
                ("box", title, None)
            }
        };
        let press = match scope {
            ComposerScope::Panel => Message::Chat(ChatMsg::MentionPick(i)),
            ComposerScope::Card { node } => Message::Canvas(CanvasMsg::Chat(
                crate::canvas_chat::CanvasChatMsg::MentionPick { node: node.clone(), index: i },
            )),
        };
        let row_content = row![
            icon(icon_name, (12.0 * s).clamp(9.0, 14.0), if selected { pal::ACCENT } else { pal::TEXT_SECONDARY }),
            text(label)
                .size((pal::fs_caption() * s).max(9.0))
                .color(if selected { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY }),
            dir.map(|d| text(d).size((pal::fs_micro() * s).max(8.0)).color(pal::TEXT_MUTED)),
        ]
        .spacing((6.0 * s).max(2.0))
        .align_y(iced::alignment::Vertical::Center);
        button(container(row_content).width(Length::Fill))
            .padding(Padding::new(10.0 * s).vertical(4.0 * s))
            .style(move |_, _| iced::widget::button::Style {
                background: selected.then(|| pal::ACCENT_SOFT.into()),
                ..Default::default()
            })
            .on_press(press)
    });
    let list = container(
        scroll::v(column(rows.map(iced::Element::from)).spacing(0).width(Length::Fill))
            .height(iced::Length::Shrink)
            .width(Length::Fill),
    )
    .max_height(224.0 * s);
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
