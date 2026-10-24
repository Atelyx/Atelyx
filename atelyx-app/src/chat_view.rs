//! AI 对话面板：会话侧栏 + 顶部行 + 输入区（发送/停止圆钮）；消息流渲染走
//! [`crate::chat_stream`] 公共段（面板与画布对话卡共用，悬浮操作按钮组经操作钩子注入）。

use iced::widget::{button, column, container, mouse_area, row, space, stack, Space};
use iced::widget::container::Style;
use iced::{border, Length, Padding};
use serde_json::{json, Value};

use crate::chat_actions::ChatMsg;
use crate::ui::icon::icon;
use crate::ui::{controls, palette as pal};
use crate::ui::scroll;
use crate::ui::text;
use crate::{App, Message};

pub fn chat_view(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
    chat_root(app, window)
}


/// 侧栏并排的最小面板宽（228 侧栏 + 对话列最低可读宽）。
const SIDEBAR_INLINE_MIN_W: f32 = 640.0;
/// 转圈动画每拍旋转角（UiTick 100ms 一拍；约 1 圈 / 秒）。
const SPIN_STEP_RAD: f32 = 0.63;

fn chat_root<'a>(app: &'a App, window: iced::window::Id) -> iced::Element<'a, Message> {
    if !app.chat.sidebar_open {
        return container(chat_column(app, window))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }
    // 宽度自适应：够宽 = 并排常驻；不足 = 盖层（点对话区收起，面板内按压不穿透）
    iced::widget::responsive(move |size| {
        if size.width >= SIDEBAR_INLINE_MIN_W {
            row![
                sidebar(app),
                // 侧栏与对话列之间的 1px 分隔（独立元素，避免 Border 单边构造差异）
                container(Space::new())
                    .width(1.0)
                    .height(Length::Fill)
                    .style(|_| container::Style {
                        background: Some(pal::BORDER.into()),
                        ..Default::default()
                    }),
                chat_column(app, window),
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            // 三层堆叠：底层全屏捕获（点对话区空白收起）→ 对话列（控件捕获时不穿透，
            // 点发送/模型钮不会连带收起侧栏）→ 侧栏面板（面板内按压吞掉）
            iced::widget::stack![
                iced::widget::mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
                    .on_press(Message::Chat(ChatMsg::ToggleSidebar)),
                container(chat_column(app, window))
                    .width(Length::Fill)
                    .height(Length::Fill),
                iced::widget::mouse_area(sidebar(app))
                    .on_press(Message::Chat(ChatMsg::OverlayPress)),
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// ===== 会话侧栏 =====

fn sidebar(app: &App) -> iced::Element<'_, Message> {
    let query = app.chat.sidebar_search.trim().to_lowercase();
    let mut listed: Vec<&Value> = app
        .chat
        .sessions
        .iter()
        .filter(|s| {
            query.is_empty()
                || s.get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("未命名对话")
                    .to_lowercase()
                    .contains(&query)
        })
        .collect();
    listed.sort_by_key(|s| -s.get("updatedAt").and_then(Value::as_i64).unwrap_or(0));
    let mut col = column![].spacing(2).padding(Padding::new(6.0));
    col = col.push(
        container(controls::form_input(
            "搜索会话",
            &app.chat.sidebar_search,
            208.0,
            |t| Message::Chat(ChatMsg::SidebarSearch(t)),
            Message::Noop,
        ))
        .padding(Padding::ZERO.bottom(6.0)),
    );
    if listed.is_empty() {
        let label = if query.is_empty() { "暂无历史会话" } else { "没有匹配的会话" };
        col = col.push(
            container(text(label).size(pal::fs_caption()).color(pal::TEXT_MUTED))
                .width(Length::Fill)
                .padding(8.0),
        );
    }
    for s in listed {
        col = col.push(session_row(app, s));
    }
    container(
        scroll::v(col).width(Length::Fill).height(Length::Fill),
    )
    .width(228.0)
    .height(Length::Fill)
    .style(|_| container::Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Default::default()
    })
    .into()
}

fn session_row<'a>(app: &'a App, s: &'a Value) -> iced::Element<'a, Message> {
    let id = s.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let title = s
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("未命名对话")
        .to_string();
    let active = app.chat.active.as_deref() == Some(id.as_str());
    let menu_open = app.chat.session_menu.as_deref() == Some(id.as_str());
    let confirm_open = app.chat.delete_confirm.as_deref() == Some(id.as_str());
    let hovered = app.chat.row_hover.as_deref() == Some(id.as_str());

    // 激活行左侧 2px 金轨（悬停 / 常态透明占位，宽度不跳）
    let rail = |color: iced::Color| {
        container(space())
            .width(Length::Fixed(2.0))
            .height(Length::Fill)
            .style(move |_t| Style {
                background: Some(color.into()),
                ..Style::default()
            })
    };
    let mut inner = row![rail(if active { pal::ACCENT } else { iced::Color::TRANSPARENT })]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);
    inner = inner.push(
        container(
            column![
                text(title)
                    .size(pal::fs_caption())
                    .color(if active { pal::ACCENT } else { pal::TEXT_PRIMARY })
                    .width(Length::Fill),
                text(rel_time(s.get("updatedAt").and_then(Value::as_i64).unwrap_or(0)))
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            ]
            .spacing(1),
        )
        .width(Length::Fill),
    );
    // 操作钮悬停显形（确认删除态常显），避免常驻按钮列挤占标题
    if confirm_open {
        inner = inner.push(
            button(text("确认删除？").size(pal::fs_micro()).color(pal::DANGER))
                .padding(4.0)
                .style(controls::row_style(false, pal::RADIUS_XS))
                .on_press(Message::Chat(ChatMsg::DeleteSession(id.clone()))),
        );
        inner = inner.push(
            button(icon("x", 12.0, pal::TEXT_MUTED))
                .padding(4.0)
                .style(controls::row_style(false, pal::RADIUS_XS))
                .on_press(Message::Chat(ChatMsg::SessionMenu(None))),
        );
    } else if hovered || menu_open {
        inner = inner.push(
            button(icon(if menu_open { "trash-2" } else { "more-horizontal" }, 12.0, pal::TEXT_MUTED))
                .padding(4.0)
                .style(controls::row_style(false, pal::RADIUS_XS))
                .on_press(if menu_open {
                    Message::Chat(ChatMsg::DeleteSession(id.clone()))
                } else {
                    Message::Chat(ChatMsg::SessionMenu(Some(id.clone())))
                }),
        );
        if menu_open {
            inner = inner.push(
                button(icon("x", 12.0, pal::TEXT_MUTED))
                    .padding(4.0)
                    .style(controls::row_style(false, pal::RADIUS_XS))
                    .on_press(Message::Chat(ChatMsg::SessionMenu(None))),
            );
        }
    }
    mouse_area(
        button(inner)
            .padding(Padding::ZERO.vertical(6.0))
            .width(Length::Fill)
            .style(controls::row_style(active, pal::RADIUS_SM))
            .on_press(Message::Chat(ChatMsg::SessionPicked(id.clone()))),
    )
    .on_enter(Message::Chat(ChatMsg::RowHover(Some(id))))
    .on_exit(Message::Chat(ChatMsg::RowHover(None)))
    .into()
}

fn rel_time(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mins = (now - ts).max(0) / 60_000;
    match mins {
        0 => "刚刚".to_string(),
        m if m < 60 => format!("{m} 分钟前"),
        h if h < 60 * 24 => format!("{} 小时前", h / 60),
        d if d < 60 * 24 * 30 => format!("{} 天前", d / (60 * 24)),
        _ => "更早".to_string(),
    }
}

// ===== 对话列 =====

fn chat_column(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
    column![
        top_bar(app, window),
        controls::h_divider_subtle(),
        message_flow(app, window),
        input_area(app, window),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn top_bar(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
    let has_error = app.chat.error.is_some() || app.chat.persist_error.is_some();
    let mut left = row![].spacing(6).align_y(iced::alignment::Vertical::Center);
    if has_error {
        let text_value = match (&app.chat.error, &app.chat.persist_error) {
            (Some(e), _) => e.clone(),
            (None, Some((m, _))) => m.clone(),
            (None, None) => String::new(),
        };
        left = left.push(icon("circle-x", 13.0, pal::DANGER));
        left = left.push(
            text(text_value)
                .size(pal::fs_caption())
                .color(pal::DANGER)
                .width(Length::Fill),
        );
    } else {
        left = left.push(icon("message-square", 13.0, pal::TEXT_SECONDARY));
        left = left.push(
            text(active_title(app))
                .size(pal::fs_caption())
                .color(pal::TEXT_PRIMARY),
        );
    }
    // 手动重新生成标题：紧贴会话标题右侧（有会话且非流式时可用；
    // 命名在途 = 转圈动画且不可再点，完成前禁止重复触发）
    let canvas_bound = app
        .chat
        .active
        .as_deref()
        .and_then(crate::canvas_chat::node_of_session)
        .is_some();
    if let Some(session_id) = app.chat.active.clone() {
        if !is_streaming(app) && !canvas_bound {
            if app.chat.renaming.contains(&session_id) {
                // 转圈：UiTick 100ms 一拍递增 spin_tick，视图按计数取旋转角
                let angle = app.chat.spin_tick as f32 * SPIN_STEP_RAD;
                left = left.push(crate::ui::icon::icon_spin(
                    "loader-circle",
                    14.0,
                    pal::ACCENT,
                    1.5,
                    angle,
                ));
            } else {
                left = left.push(controls::icon_button(
                    "refresh-cw",
                    12.0,
                    22.0,
                    controls::Variant::Ghost,
                    Some(Message::Chat(ChatMsg::RegenTitle)),
                    Some((window, "重新生成标题".to_string())),
                ));
            }
        }
    }
    left = left.push(Space::new().width(Length::Fill));
    // 压缩会话历史只对「有内容的会话」有意义：空会话 / 无会话时不出现（免成无意义按钮）
    let compactions = app
        .chat
        .active
        .as_ref()
        .and_then(|id| app.chat.sessions.iter().find(|s| s["id"] == json!(id)))
        .map(|s| s.get("messages").and_then(Value::as_array).map(|m| m.len()).unwrap_or(0) >= 2)
        .unwrap_or(false);
    let mut right = row![
        controls::icon_button(
            "file-plus",
            13.0,
            24.0,
            controls::Variant::Ghost,
            Some(Message::Chat(ChatMsg::NewSession)),
            Some((window, "新建会话".to_string())),
        ),
        controls::icon_button(
            "history",
            13.0,
            24.0,
            controls::Variant::Ghost,
            Some(Message::Chat(ChatMsg::ToggleSidebar)),
            Some((window, "历史会话".to_string())),
        ),
    ]
    .spacing(4);
    if compactions {
        right = right.push(if app.chat.compacting.is_some() {
            controls::icon_button("loader-circle", 13.0, 24.0, controls::Variant::Ghost, None, None)
        } else {
            controls::icon_button(
                "layers",
                13.0,
                24.0,
                controls::Variant::Ghost,
                Some(Message::Chat(ChatMsg::Compact)),
                Some((window, "压缩会话历史（折叠为摘要，原文保留）".to_string())),
            )
        });
    }
    if has_error {
        right = right.push(controls::icon_button(
            "x",
            13.0,
            24.0,
            controls::Variant::Ghost,
            Some(Message::Chat(ChatMsg::ClearError)),
            Some((window, "清除提示".to_string())),
        ));
    }
    container(
        row![container(left).width(Length::Fill), right]
            .spacing(6)
            .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::new(10.0).vertical(6.0))
    .style(|_| container::Style {
        background: Some(pal::BG_SECONDARY.into()),
        ..Default::default()
    })
    .into()
}

fn active_title(app: &App) -> String {
    match &app.chat.active {
        Some(id) => {
            // 画布卡会话：标题取卡（内嵌 title / 首条 user 消息前缀）
            if let Some(node) = crate::canvas_chat::node_of_session(id) {
                return app
                    .canvas
                    .as_ref()
                    .and_then(|s| s.data.nodes.iter().find(|n| n.id == node))
                    .map(crate::canvas_chat::card_title)
                    .unwrap_or_else(|| "画布对话".to_string());
            }
            app.chat
                .sessions
                .iter()
                .find(|s| s["id"] == json!(id))
                .and_then(|s| s.get("title").and_then(Value::as_str))
                .unwrap_or("新对话")
                .to_string()
        }
        None => "新对话".to_string(),
    }
}

fn is_streaming(app: &App) -> bool {
    let Some(id) = app.chat.active.as_deref() else {
        return false;
    };
    if let Some(node) = crate::canvas_chat::node_of_session(id) {
        return crate::canvas_chat::is_streaming(app, node);
    }
    app.chat.streaming.contains(id)
}

// ===== 消息流 =====

fn message_flow(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
    let Some(session_id) = app.chat.active.clone() else {
        return empty_flow();
    };
    // 画布卡会话：消息 / 压缩注解直接取节点数据；展开集也归卡（双视图同步）
    let canvas_node = crate::canvas_chat::node_of_session(&session_id).map(str::to_string);
    let (session, messages, canvas_view) = if let Some(node_id) = canvas_node.as_deref() {
        let Some(session) = app.canvas.as_ref() else { return empty_flow() };
        let Some(node) = session.data.nodes.iter().find(|n| n.id == node_id) else {
            return empty_flow();
        };
        let Some(messages) = node.data.get("messages").and_then(Value::as_array) else {
            return empty_flow();
        };
        (&node.data, messages.as_slice(), Some(&session.view))
    } else {
        let Some(session) = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id)) else {
            return empty_flow();
        };
        let Some(messages) = session.get("messages").and_then(Value::as_array) else {
            return empty_flow();
        };
        (session, messages.as_slice(), None)
    };
    if messages.is_empty() {
        return empty_flow();
    }
    // 流式态只指向末条消息（渲染段按消息 id 判定，仅 assistant 块消费该标记）
    let streaming = is_streaming(app);
    let streaming_id = streaming
        .then(|| messages.last())
        .flatten()
        .and_then(|m| m.get("id").and_then(Value::as_str));
    let actions: crate::chat_stream::ActionsHook<'_> =
        Box::new(move |m, is_assistant| action_row(app, window, m, is_assistant));
    let ctx = crate::chat_stream::StreamCtx {
        docs: &app.chat.docs,
        images: &app.note_images,
        expanded: canvas_view.map_or(&app.chat.expanded, |v| &v.expanded),
        theme: &app.theme,
        scale: 1.0,
        actions: Some(actions),
        streaming_id,
        toggle: if canvas_node.is_some() {
            crate::chat_stream::ToggleRoute::Canvas
        } else {
            crate::chat_stream::ToggleRoute::Panel
        },
        selectable: canvas_node.is_none(),
        selection: canvas_node.is_none().then_some(app.chat.selection.as_ref()).flatten(),
    };
    let col = crate::chat_stream::message_column(messages, session, ctx);
    // 覆盖层恒定 Stack 承载、按需追加：显隐不换型（Scrollable ↔ Stack 换型会整树重建、丢滚动位置）
    let flow = mouse_area(
        scroll::v(container(col).width(Length::Fill))
            .id(crate::chat_actions::chat_flow_id())
            .on_scroll(|v| Message::Chat(ChatMsg::FlowScrolled(v.relative_offset().y)))
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_right_press(crate::layout_view::selection_popup(
        window,
        app.cursor_for(window),
        crate::markdown_select::SelectionTarget::Chat,
    ));
    let mut layered = stack![flow].width(Length::Fill).height(Length::Fill);
    if !app.chat.follow_bottom {
        // 上翻即脱离跟随：浮钮一键回底（贴底状态由 on_scroll 恢复）
        layered = layered.push(
            container(
                button(
                    row![
                        icon("chevron-down", 11.0, pal::TEXT_SECONDARY),
                        text("回到底部".to_string()).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                    ]
                    .spacing(4)
                )
                .padding(Padding::new(10.0).vertical(5.0))
                .style(controls::row_style(false, 999.0))
                .on_press(Message::Chat(ChatMsg::ScrollToBottom)),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Bottom)
            .padding(Padding::ZERO.bottom(12.0)),
        );
    }
    layered.into()
}

fn empty_flow() -> iced::Element<'static, Message> {
    controls::empty_state("message-square", "开始新的 AI 对话", "", None)
}

/// 消息操作按钮组（复制 / 回到此处 / 重新生成；条件与对话面板约定一致）。
fn action_row<'a>(
    app: &'a App,
    window: iced::window::Id,
    m: &Value,
    is_assistant: bool,
) -> iced::Element<'a, Message> {
    let content = m.get("content").and_then(Value::as_str).unwrap_or("");
    let display = m
        .get("displayContent")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(content);
    let message_id = m.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let mut row = row![].spacing(2);
    if !display.is_empty() {
        let payload = display.to_string();
        row = row.push(controls::icon_button(
            "copy",
            12.0,
            22.0,
            controls::Variant::Ghost,
            Some(Message::CopyCode(payload)),
            Some((window, "复制".to_string())),
        ));
    }
    if is_assistant && !content.is_empty() && !content.starts_with("[错误]") {
        row = row.push(controls::icon_button(
            "corner-up-left",
            12.0,
            22.0,
            controls::Variant::Ghost,
            Some(Message::Chat(ChatMsg::Rollback(message_id.clone()))),
            Some((window, "回到此处（该消息之后转为新话题）".to_string())),
        ));
        // 重新生成回复常驻显示（中断 / 历史消息位置不消失）；仅在「最后一条 + 非流式」时可点
        let actionable = is_last_message(app, m) && !is_streaming(app);
        row = row.push(controls::icon_button(
            "refresh-cw",
            12.0,
            22.0,
            controls::Variant::Ghost,
            actionable.then(|| Message::Chat(ChatMsg::Regenerate)),
            Some((window, "重新生成回复".to_string())),
        ));
    }
    if app.chat.rollback_confirm.as_deref() == Some(message_id.as_str()) {
        row = row.push(
            button(text("确认回滚？").size(pal::fs_micro()).color(pal::DANGER))
                .padding(4.0)
                .style(controls::row_style(false, pal::RADIUS_XS))
                .on_press(Message::Chat(ChatMsg::Rollback(message_id.clone()))),
        );
        row = row.push(
            button(text("取消").size(pal::fs_micro()).color(pal::TEXT_MUTED))
                .padding(4.0)
                .style(controls::row_style(false, pal::RADIUS_XS))
                .on_press(Message::Chat(ChatMsg::RollbackCancel)),
        );
    }
    row.into()
}

/// 该消息是否为当前会话的最后一条（重新生成回复按钮的可点条件之一）。
fn is_last_message(app: &App, m: &Value) -> bool {
    let Some(session_id) = &app.chat.active else {
        return false;
    };
    let last = if crate::canvas_chat::node_of_session(session_id).is_some() {
        crate::canvas_chat::messages_of(app, session_id).and_then(|ms| ms.last())
    } else {
        app.chat
            .sessions
            .iter()
            .find(|s| s["id"] == json!(session_id))
            .and_then(|s| s.get("messages").and_then(Value::as_array))
            .and_then(|ms| ms.last())
    };
    last.map(|last| {
        last.get("id").and_then(Value::as_str) == m.get("id").and_then(Value::as_str)
    })
    .unwrap_or(false)
}

// ===== 输入区 =====

fn input_area(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
    let streaming = is_streaming(app);
    let send_btn = if streaming {
        controls::icon_button(
            "square",
            14.0,
            28.0,
            controls::Variant::Secondary,
            Some(Message::Chat(ChatMsg::Stop)),
            Some((window, "停止生成".to_string())),
        )
    } else {
        controls::icon_button(
            "arrow-up",
            14.0,
            28.0,
            controls::Variant::Primary,
            Some(Message::Chat(ChatMsg::Send)),
            Some((window, "发送（Enter）".to_string())),
        )
    };
    // 工具排（盒子内底部，与输入面同属一块、不画分隔线）：左 + / Agent / 模型；右 发送 / 停止
    let plus_open = app
        .popup
        .as_ref()
        .is_some_and(|(_, open)| matches!(open.state, crate::layout_view::PopupState::ChatPlusMenu));
    let plus_btn = button(icon("plus", 13.0, if plus_open { pal::ACCENT } else { pal::TEXT_SECONDARY }))
        .padding(Padding::new(6.0).vertical(4.0))
        .style(controls::row_style(false, pal::RADIUS_XS))
        .on_press(crate::layout_view::btn_popup_min(
            window,
            app.cursor_for(window),
            crate::layout_view::PopupState::ChatPlusMenu,
            0.0,
        ));
    // 画布卡会话：Agent / 模型由卡节点配置，面板侧不再给选择入口
    let canvas_bound = app
        .chat
        .active
        .as_deref()
        .and_then(crate::canvas_chat::node_of_session)
        .is_some();
    let tool_row = if canvas_bound {
        row![
            text("画布对话卡".to_string())
                .size(pal::fs_micro())
                .color(pal::TEXT_MUTED),
            Space::new().width(Length::Fill),
            send_btn,
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center)
    } else {
        row![
            plus_btn,
            agent_trigger(app, window),
            model_select_trigger(app, window),
            Space::new().width(Length::Fill),
            send_btn,
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center)
    };

    // 自绘输入面（# 标签高亮 / 整标签交互由部件托管）
    let placeholder = if canvas_bound {
        "输入消息到画布对话卡，Enter 发送"
    } else {
        "输入消息，Enter 发送，Shift+Enter 换行"
    };
    let surface = crate::chat_input::Input::new(
        &app.chat.input,
        &app.chat.mention_refs,
        crate::chat_input::InputRoute::Panel,
    )
    .picker_open(app.chat.mention_picker.is_some())
    .placeholder(placeholder);
    let surface = container(iced::Element::new(surface))
        .width(Length::Fill)
        .padding(Padding::ZERO.left(2.0).right(2.0).top(8.0));

    // 输入盒内纵向三段：# 候选浮层（打开时）→ 输入面 → 工具排
    let mut inner = column![].spacing(0).width(Length::Fill);
    if let Some(picker) = &app.chat.mention_picker {
        inner = inner.push(mention_picker_view(picker));
    }
    inner = inner.push(surface);
    inner = inner.push(container(tool_row).width(Length::Fill).padding(Padding::ZERO.top(4.0)));
    let input_box = container(inner)
        .width(Length::Fill)
        .padding(Padding::new(8.0))
        .style(|_| container::Style {
            background: Some(pal::BG_SUNKEN.into()),
            border: border::Border {
                width: 0.5,
                radius: pal::RADIUS_MD.into(),
                color: pal::BORDER,
            },
            ..Default::default()
        });

    // 输入区不铺底色：只一条 1px 上边线 + 图片托盘行（非空时）+ 输入盒本体
    let mut area = column![controls::h_divider_subtle()].width(Length::Fill);
    if !app.chat.pending_images.is_empty() {
        area = area.push(image_tray(app));
    }
    area = area.push(
        container(input_box)
            .width(Length::Fill)
            .padding(Padding::new(12.0).top(8.0).bottom(10.0)),
    );
    container(area).width(Length::Fill).into()
}

/// # 引用候选浮层：输入面上方弹出（锚在输入盒顶部，随候选行数自高）。
fn mention_picker_view<'a>(
    picker: &'a crate::chat_actions::MentionPicker,
) -> iced::Element<'a, Message> {
    let rows = picker.candidates.iter().enumerate().map(|(i, target)| {
        let selected = i == picker.selected;
        let (icon_name, label, dir) = match target {
            crate::chat_actions::MentionTarget::Vault { rel, is_dir } => {
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
            // 面板不列画布卡候选（该入口只在画布卡输入面出现）
            crate::chat_actions::MentionTarget::Card { node_id } => ("box", node_id.clone(), None),
        };
        let press = Message::Chat(ChatMsg::MentionPick(i));
        let row_content = row![
            icon(icon_name, 12.0, if selected { pal::ACCENT } else { pal::TEXT_SECONDARY }),
            text(label)
                .size(pal::fs_caption())
                .color(if selected { pal::TEXT_PRIMARY } else { pal::TEXT_SECONDARY }),
            dir.map(|d| text(d).size(pal::fs_micro()).color(pal::TEXT_MUTED)),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);
        button(container(row_content).width(Length::Fill))
            .padding(Padding::new(10.0).vertical(4.0))
            .style(move |_, _| iced::widget::button::Style {
                background: if selected { Some(pal::ACCENT_SOFT.into()) } else { None },
                ..Default::default()
            })
            .on_press(press)
    });
    let list = container(
        scroll::v(column(rows.map(iced::Element::from)).spacing(0).width(Length::Fill))
            .height(iced::Length::Shrink)
            .width(Length::Fill),
    )
    .max_height(224.0);
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
fn image_tray(app: &App) -> iced::Element<'_, Message> {
    let chips = app.chat.pending_images.iter().enumerate().map(|(i, img)| {
        let remove = Message::Chat(ChatMsg::RemoveAttachment(i));
        let name = img.file_name.clone();
        button(
            row![
                icon("image", 12.0, pal::TEXT_SECONDARY),
                text(name).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                icon("x", 10.0, pal::TEXT_MUTED),
            ]
            .spacing(4)
            .align_y(iced::alignment::Vertical::Center),
        )
        .padding(Padding::new(8.0).vertical(3.0))
        .style(controls::row_style(false, pal::RADIUS_XS))
        .on_press(remove)
    });
    container(scroll::v(row(chips.map(iced::Element::from)).spacing(6)))
        .width(Length::Fill)
        .padding(Padding::new(12.0).top(8.0).bottom(0.0))
        .into()
}

fn agent_label(app: &App) -> String {
    let id = app.chat.active_agent_id();
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

fn agent_trigger(app: &App, window: iced::window::Id) -> iced::Element<'static, Message> {
    button(
        row![
            icon("bot", 12.0, pal::TEXT_SECONDARY),
            text(agent_label(app)).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
            icon("chevron-down", 10.0, pal::TEXT_MUTED),
        ]
        .spacing(4)
        .align_y(iced::alignment::Vertical::Center),
    )
    .padding(Padding::new(6.0).vertical(3.0))
    .style(controls::row_style(false, pal::RADIUS_XS))
    .on_press(crate::layout_view::btn_popup_min(
        window,
        app.cursor_for(window),
        crate::layout_view::PopupState::ChatAgentMenu,
        0.0,
    ))
    .into()
}

/// 模型/推理等级复合触发钮：显示「模型 · 等级」，两级菜单分别设置（面板级覆盖，正交）。
fn model_select_trigger(app: &App, window: iced::window::Id) -> iced::Element<'static, Message> {
    button(
        row![
            icon("sparkles", 12.0, pal::TEXT_SECONDARY),
            text(format!(
                "{} · {}",
                crate::layout_view::chat_model_trigger_label(app),
                crate::layout_view::chat_effort_label(app)
            ))
            .size(pal::fs_micro())
            .color(pal::TEXT_SECONDARY),
            icon("chevron-down", 10.0, pal::TEXT_MUTED),
        ]
        .spacing(4)
        .align_y(iced::alignment::Vertical::Center),
    )
    .padding(Padding::new(6.0).vertical(3.0))
    .style(controls::row_style(false, pal::RADIUS_XS))
    .on_press(crate::layout_view::btn_popup_min(
        window,
        app.cursor_for(window),
        crate::layout_view::PopupState::ChatModelMenu,
        0.0,
    ))
    .into()
}
