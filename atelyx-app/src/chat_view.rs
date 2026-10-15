//! AI 对话面板：会话侧栏 + 顶部行 + 消息流 + 输入区（user 气泡右对齐、assistant
//! 无气泡全宽、思考/工具步折叠、悬浮操作按钮组、发送/停止圆钮）。

use iced::widget::{button, column, container, row, scrollable, stack, text, Space};
use iced::{border, Length, Padding};
use serde_json::{json, Value};

use crate::chat_actions::ChatMsg;
use crate::ui::icon::icon;
use crate::ui::{controls, palette as pal};
use crate::{App, Message};

pub fn chat_view(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
    let _ = window;
    chat_root(app)
}


fn chat_root(app: &App) -> iced::Element<'_, Message> {
    let mut root = row![].height(Length::Fill);
    if app.chat.sidebar_open {
        root = root.push(sidebar(app));
        // 侧栏与对话列之间的 1px 分隔（独立元素，避免 Border 单边构造差异）
        root = root.push(
            container(Space::new())
                .width(1.0)
                .height(Length::Fill)
                .style(|_| container::Style {
                    background: Some(pal::BORDER.into()),
                    ..Default::default()
                }),
        );
    }
    root = root.push(chat_column(app));
    container(root)
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
        scrollable(col).width(Length::Fill).height(Length::Fill),
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
    let mut inner = row![].spacing(4).align_y(iced::alignment::Vertical::Center);
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
    if app.chat.delete_confirm.as_deref() == Some(id.as_str()) {
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
    } else if app.chat.session_menu.as_deref() == Some(id.as_str()) {
        inner = inner.push(
            button(icon("trash-2", 12.0, pal::DANGER))
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
    } else {
        inner = inner.push(
            button(icon("more-horizontal", 12.0, pal::TEXT_MUTED))
                .padding(4.0)
                .style(controls::row_style(false, pal::RADIUS_XS))
                .on_press(Message::Chat(ChatMsg::SessionMenu(Some(id.clone())))),
        );
    }
    button(inner)
        .padding(6.0)
        .width(Length::Fill)
        .style(controls::row_style(active, pal::RADIUS_SM))
        .on_press(Message::Chat(ChatMsg::SessionPicked(id)))
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

fn chat_column(app: &App) -> iced::Element<'_, Message> {
    column![
        top_bar(app),
        controls::h_divider_subtle(),
        message_flow(app),
        input_area(app),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn top_bar(app: &App) -> iced::Element<'_, Message> {
    let has_error = app.chat.error.is_some() || app.chat.persist_error.is_some();
    let title_text: String = match (&app.chat.error, &app.chat.persist_error) {
        (Some(e), _) => e.clone(),
        (None, Some((m, _))) => m.clone(),
        (None, None) => active_title(app),
    };
    let title_color = if has_error { pal::DANGER } else { pal::TEXT_PRIMARY };
    let right = row![
        controls::icon_button(
            "file-plus",
            13.0,
            24.0,
            controls::Variant::Ghost,
            Some(Message::Chat(ChatMsg::NewSession)),
            None,
        ),
        controls::icon_button(
            "history",
            13.0,
            24.0,
            controls::Variant::Ghost,
            Some(Message::Chat(ChatMsg::ToggleSidebar)),
            None,
        ),
        if app.chat.active.is_some() {
            controls::icon_button(
                "pen-line",
                13.0,
                24.0,
                controls::Variant::Ghost,
                Some(Message::Chat(ChatMsg::Rename)),
                None,
            )
        } else {
            controls::icon_button("pen-line", 13.0, 24.0, controls::Variant::Ghost, None, None)
        },
        if app.chat.compacting.is_some() {
            controls::icon_button("loader", 13.0, 24.0, controls::Variant::Ghost, None, None)
        } else {
            controls::icon_button(
                "layers",
                13.0,
                24.0,
                controls::Variant::Ghost,
                Some(Message::Chat(ChatMsg::Compact)),
                None,
            )
        },
    ]
    .spacing(4);
    container(
        row![
            icon("message-square", 13.0, pal::TEXT_SECONDARY),
            text(title_text)
                .size(pal::fs_caption())
                .color(title_color)
                .width(Length::Fill),
            right,
        ]
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
        Some(id) => app
            .chat
            .sessions
            .iter()
            .find(|s| s["id"] == json!(id))
            .and_then(|s| s.get("title").and_then(Value::as_str))
            .unwrap_or("新对话")
            .to_string(),
        None => "新对话".to_string(),
    }
}

fn is_streaming(app: &App) -> bool {
    app.chat
        .active
        .as_deref()
        .map(|id| app.chat.streaming.contains(id))
        .unwrap_or(false)
}

// ===== 消息流 =====

fn message_flow(app: &App) -> iced::Element<'_, Message> {
    let Some(session_id) = app.chat.active.clone() else {
        return empty_flow();
    };
    let Some(session) = app.chat.sessions.iter().find(|s| s["id"] == json!(session_id)) else {
        return empty_flow();
    };
    let Some(messages) = session.get("messages").and_then(Value::as_array) else {
        return empty_flow();
    };
    if messages.is_empty() {
        return empty_flow();
    }
    let streaming = is_streaming(app);
    let last_idx = messages.len() - 1;
    let mut col = column![].spacing(10).padding(Padding::new(12.0));
    if let Some(marker) = compaction_marker(session) {
        col = col.push(marker);
    }
    for (i, m) in messages.iter().enumerate() {
        let is_streaming_msg = streaming && i == last_idx && m["role"] == json!("assistant");
        match m["role"].as_str() {
            Some("user") => col = col.push(user_bubble(app, m)),
            Some("assistant") => col = col.push(assistant_block(app, m, is_streaming_msg)),
            _ => {}
        }
    }
    scrollable(container(col).width(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn empty_flow() -> iced::Element<'static, Message> {
    controls::empty_state("message-square", "开始新的 AI 对话", "", None)
}

/// 压缩注解标记（锚点为最后一条消息时渲染于列表末尾）。
fn compaction_marker(session: &Value) -> Option<iced::Element<'static, Message>> {
    let c = session.get("compaction")?;
    if c.is_null() {
        return None;
    }
    let summary = c.get("summary").and_then(Value::as_str).unwrap_or("");
    let count = c.get("messageCount").and_then(Value::as_u64).unwrap_or(0);
    let mut col = column![].spacing(4);
    col = col.push(
        text(format!("上下文已压缩 · 以上 {count} 条已折叠为摘要"))
            .size(pal::fs_micro())
            .color(pal::TEXT_MUTED),
    );
    if !summary.is_empty() {
        col = col.push(
            text(summary.to_string()).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
        );
    }
    Some(
        container(col)
            .width(Length::Fill)
            .padding(8.0)
            .style(|_| container::Style {
                border: border::Border {
                    width: 0.5,
                    radius: pal::RADIUS_MD.into(),
                    color: pal::BORDER,
                },
                ..Default::default()
            })
            .into(),
    )
}

/// user 气泡（右对齐、max-width 85%、displayContent 优先）。
fn user_bubble<'a>(app: &'a App, m: &'a Value) -> iced::Element<'a, Message> {
    let source = m
        .get("displayContent")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| m.get("content").and_then(Value::as_str).unwrap_or(""));
    let mut col = column![].spacing(2);
    col = col.push(
        container(markdown_doc(app, source))
            .max_width(560.0)
            .padding(10.0)
            .style(|_| container::Style {
                background: Some(pal::BG_TERTIARY.into()),
                border: border::Border {
                    width: 0.5,
                    radius: pal::RADIUS_MD.into(),
                    color: pal::BORDER,
                },
                ..Default::default()
            }),
    );
    col = col.push(action_row(app, m, false));
    container(container(col).align_x(iced::alignment::Horizontal::Right).width(Length::Fill))
        .width(Length::Fill)
        .into()
}

/// assistant 块（无气泡全宽）：steps（思考折叠 / 工具行 / 叙述行）+ 最终正文 + 操作。
fn assistant_block<'a>(app: &'a App, m: &'a Value, streaming: bool) -> iced::Element<'a, Message> {
    let content = m.get("content").and_then(Value::as_str).unwrap_or("");
    let steps = m.get("steps").and_then(Value::as_array);
    let mut col = column![].spacing(6);
    if let Some(steps) = steps {
        for s in steps {
            match s.get("kind").and_then(Value::as_str) {
                Some("reasoning") => col = col.push(thinking_block(s, streaming)),
                Some("text") => {
                    let t = s.get("text").and_then(Value::as_str).unwrap_or("");
                    if !t.is_empty() {
                        col = col.push(markdown_doc(app, t));
                    }
                }
                Some("tool") => col = col.push(tool_row(s)),
                _ => {}
            }
        }
    }
    let has_tools = steps
        .map(|ss| ss.iter().any(|s| s.get("kind") == Some(&json!("tool"))))
        .unwrap_or(false);
    if has_tools && !content.is_empty() {
        col = col.push(controls::h_divider_subtle());
    }
    if !content.is_empty() {
        col = col.push(markdown_doc(app, content));
    } else if streaming && steps.map(|ss| ss.is_empty()).unwrap_or(true) {
        col = col.push(text("生成中…").size(pal::fs_caption()).color(pal::TEXT_MUTED));
    }
    col = col.push(action_row(app, m, true));
    container(col).width(Length::Fill).into()
}

/// 思考折叠块（流式态摘要 = 最新一行）。
fn thinking_block(s: &Value, streaming: bool) -> iced::Element<'static, Message> {
    let full = s.get("text").and_then(Value::as_str).unwrap_or("");
    let summary = full
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    let header = if streaming && !summary.is_empty() {
        format!("思考过程 · {summary}")
    } else {
        "思考过程".to_string()
    };
    details_block("brain", header, full.to_string(), "思考内容为空")
}

/// 工具行（图标 + 参数摘要 + 结果摘要 + 状态；展开参数与结果）。
fn tool_row(s: &Value) -> iced::Element<'static, Message> {
    let run = s.get("run").cloned().unwrap_or(Value::Null);
    let name = run.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
    let status = run.get("status").and_then(Value::as_str).unwrap_or("running");
    let args_summary = run.get("argsSummary").and_then(Value::as_str).unwrap_or("");
    let result_summary = run.get("resultSummary").and_then(Value::as_str).unwrap_or("");
    let args_json = run.get("args").map(pretty_json).unwrap_or_default();
    let result = run.get("result").and_then(Value::as_str).unwrap_or("");
    let detail = format!(
        "参数\n{}\n\n结果\n{}",
        if args_json.is_empty() { "—" } else { args_json.as_str() },
        if status == "running" {
            "进行中…".to_string()
        } else if result.is_empty() {
            result_summary.to_string()
        } else {
            result.to_string()
        }
    );
    details_block(
        tool_icon(&name),
        format!("{name} · {args_summary} · {result_summary}"),
        detail,
        "",
    )
}

fn pretty_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

fn tool_icon(name: &str) -> &'static str {
    match name {
        "web_search" => "globe",
        "web_fetch" => "link",
        "read_file" | "read_history" => "file-text",
        "glob" | "grep" => "search",
        "edit_file" => "pen-line",
        "write_file" | "append_file" => "file-plus",
        _ => "wrench",
    }
}

/// 可展开详情块骨架（思考 / 工具共用；默认收起 → 本层以紧凑小字号常显）。
fn details_block(
    icon_name: &str,
    header: String,
    body: String,
    empty_hint: &str,
) -> iced::Element<'static, Message> {
    let mut col = column![].spacing(4);
    col = col.push(
        row![
            icon(icon_name, 12.0, pal::TEXT_MUTED),
            text(header).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center),
    );
    if !body.is_empty() {
        col = col.push(
            scrollable(
                container(text(body).size(pal::fs_micro()).color(pal::TEXT_SECONDARY))
                    .width(Length::Fill),
            )
            .height(140.0),
        );
    } else if !empty_hint.is_empty() {
        col = col.push(text(empty_hint.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED));
    }
    container(col)
        .width(Length::Fill)
        .padding(6.0)
        .style(|_| container::Style {
            background: Some(pal::BG_SUNKEN.into()),
            border: border::Border {
                width: 0.5,
                radius: pal::RADIUS_SM.into(),
                color: pal::BORDER,
            },
            ..Default::default()
        })
        .into()
}

/// 消息操作按钮组（复制 / 回到此处 / 重新生成；条件与对话面板约定一致）。
fn action_row<'a>(app: &'a App, m: &'a Value, is_assistant: bool) -> iced::Element<'a, Message> {
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
            None,
        ));
    }
    if is_assistant && !content.is_empty() && !content.starts_with("[错误]") {
        row = row.push(controls::icon_button(
            "corner-up-left",
            12.0,
            22.0,
            controls::Variant::Ghost,
            Some(Message::Chat(ChatMsg::Rollback(message_id.clone()))),
            None,
        ));
        if is_last_message(app, m) {
            row = row.push(controls::icon_button(
                "refresh-cw",
                12.0,
                22.0,
                controls::Variant::Ghost,
                Some(Message::Chat(ChatMsg::Regenerate)),
                None,
            ));
        }
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

fn is_last_message(app: &App, m: &Value) -> bool {
    let Some(session_id) = &app.chat.active else {
        return false;
    };
    app.chat
        .sessions
        .iter()
        .find(|s| s["id"] == json!(session_id))
        .and_then(|s| s.get("messages").and_then(Value::as_array))
        .and_then(|ms| ms.last())
        .map(|last| {
            last.get("id").and_then(Value::as_str) == m.get("id").and_then(Value::as_str)
                && !m.get("content").and_then(Value::as_str).unwrap_or("").is_empty()
        })
        .unwrap_or(false)
}

// ===== 输入区 =====

fn input_area(app: &App) -> iced::Element<'_, Message> {
    let streaming = is_streaming(app);
    let send_btn = if streaming {
        controls::icon_button(
            "square",
            14.0,
            28.0,
            controls::Variant::Secondary,
            Some(Message::Chat(ChatMsg::Stop)),
            None,
        )
    } else {
        controls::icon_button(
            "arrow-up",
            14.0,
            28.0,
            controls::Variant::Primary,
            Some(Message::Chat(ChatMsg::Send)),
            None,
        )
    };
    let tool_row = row![
        agent_trigger(&agent_label(app)),
        Space::new().width(Length::Fill),
        send_btn,
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    let input_box = container(
        iced::widget::text_editor(&app.chat.input)
            .placeholder("输入消息…")
            .on_action(|a| Message::Chat(ChatMsg::InputAction(a)))
            .height(110.0),
    )
    .width(Length::Fill)
    .padding(8.0)
    .style(|_| container::Style {
        background: Some(pal::BG_SUNKEN.into()),
        border: border::Border {
            width: 0.5,
            radius: pal::RADIUS_MD.into(),
            color: pal::BORDER,
        },
        ..Default::default()
    });
    let base = column![
        container(tool_row)
            .width(Length::Fill)
            .padding(Padding::ZERO.bottom(10.0)),
        input_box,
    ]
    .spacing(6);
    if app.chat.agent_menu_open {
        // 浮层锚定在输入区上方（底部对齐左侧）
        stack![
            container(column![Space::new().height(Length::Fill), base].width(Length::Fill))
                .width(Length::Fill),
            container(agent_menu(app))
                .align_x(iced::alignment::Horizontal::Left)
                .align_y(iced::alignment::Vertical::Bottom)
                .padding(Padding::new(8.0).bottom(150.0))
        ]
        .into()
    } else {
        container(
            column![controls::h_divider_subtle(), container(base).padding(Padding::new(12.0).top(6.0))]
                .width(Length::Fill),
        )
        .width(Length::Fill)
        .into()
    }
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

fn agent_trigger(label: &str) -> iced::Element<'static, Message> {
    button(
        row![
            icon("bot", 12.0, pal::TEXT_SECONDARY),
            text(label.to_string()).size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
            icon("chevron-down", 10.0, pal::TEXT_MUTED),
        ]
        .spacing(4)
        .align_y(iced::alignment::Vertical::Center),
    )
    .padding(Padding::new(6.0).vertical(3.0))
    .style(controls::row_style(false, pal::RADIUS_XS))
    .on_press(Message::Chat(ChatMsg::AgentMenu(true)))
    .into()
}

fn agent_menu(app: &App) -> iced::Element<'static, Message> {
    let mut col = column![].spacing(2).padding(6.0);
    let current = app.chat.active_agent_id();
    let mut entries: Vec<(Option<String>, String)> =
        app.agents.iter().map(|a| (Some(a.id.clone()), a.name.clone())).collect();
    entries.push((None, "对话（默认）".to_string()));
    for (id, name) in entries {
        let selected = current == id;
        col = col.push(
            button(
                row![
                    if selected {
                        icon("check", 12.0, pal::ACCENT)
                    } else {
                        Space::new().width(12.0).into()
                    },
                    text(name).size(pal::fs_caption()),
                ]
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center),
            )
            .width(Length::Fill)
            .padding(Padding::new(6.0).vertical(4.0))
            .style(controls::row_style(selected, pal::RADIUS_SM))
            .on_press(Message::Chat(ChatMsg::AgentPick(id.clone()))),
        );
    }
    container(col)
        .width(220.0)
        .style(|_| container::Style {
            background: Some(pal::BG_PRIMARY.into()),
            border: border::Border {
                width: 0.5,
                radius: pal::RADIUS_MD.into(),
                color: pal::BORDER_STRONG,
            },
            shadow: iced::Shadow {
                color: iced::Color::BLACK.scale_alpha(0.4),
                offset: iced::Vector::new(0.0, 4.0),
                blur_radius: 12.0,
            },
            ..Default::default()
        })
        .into()
}

/// 对话消息 markdown 渲染（复用阅读面管线；解析缓存由 ChatState 维护，
/// 未命中回落纯文本）。图片暂不参与（对话消息内嵌图片走后续批次）。
fn markdown_doc<'a>(app: &'a App, source: &'a str) -> iced::Element<'a, Message> {
    static EMPTY_IMAGES: std::sync::OnceLock<crate::markdown_view::ImageHandles> =
        std::sync::OnceLock::new();
    let images = EMPTY_IMAGES.get_or_init(std::collections::HashMap::new);
    match app.chat.docs.get(&crate::chat_actions::source_hash(source)) {
        Some(doc) => crate::markdown_view::document_view(doc, &app.theme, images, true),
        None => text(source.to_string())
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .into(),
    }
}
