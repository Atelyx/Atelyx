//! AI 对话面板：会话侧栏 + 顶部行 + 消息流 + 输入区（user 气泡右对齐、assistant
//! 无气泡全宽、思考/工具步折叠、悬浮操作按钮组、发送/停止圆钮）。

use iced::widget::{button, column, container, mouse_area, row, scrollable, space, stack, text, Space};
use iced::widget::container::Style;
use iced::{border, Length, Padding};
use serde_json::{json, Value};

use crate::chat_actions::ChatMsg;
use crate::ui::icon::icon;
use crate::ui::{controls, palette as pal};
use crate::{App, Message};

pub fn chat_view(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
    chat_root(app, window)
}


/// 侧栏并排的最小面板宽（228 侧栏 + 对话列最低可读宽）。
const SIDEBAR_INLINE_MIN_W: f32 = 640.0;
/// 转圈动画每拍旋转角（UiTick 100ms 一拍；约 1 圈 / 秒）。
const SPIN_STEP_RAD: f32 = 0.63;
/// 折叠态摘要的单行字符上限（中英文混排按字符计）。
const SUMMARY_LINE_CHARS: usize = 56;

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

/// 单行纯文本（无行内 / 块级标记）= 可直接按文本渲染并按内容收缩。
fn is_plain_line(source: &str) -> bool {
    !source.contains('\n')
        && source.chars().count() <= 200
        && !source.contains(['*', '_', '`', '[', ']', '#', '>', '|', '\\', '~'])
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
    if let Some(session_id) = app.chat.active.clone() {
        if !is_streaming(app) {
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

fn message_flow(app: &App, window: iced::window::Id) -> iced::Element<'_, Message> {
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
    // 压缩标记插在折叠块之后、首条未折叠消息之前（锚点由注解携带）
    let marker_idx = compaction_index(session);
    let mut col = column![].spacing(10).padding(Padding::new(12.0));
    for (i, m) in messages.iter().enumerate() {
        if Some(i) == marker_idx {
            if let Some(marker) = compaction_marker(session) {
                col = col.push(marker);
            }
        }
        let is_streaming_msg = streaming && i == last_idx && m["role"] == json!("assistant");
        match m["role"].as_str() {
            Some("user") => col = col.push(user_bubble(app, window, m)),
            Some("assistant") => col = col.push(assistant_block(app, window, m, is_streaming_msg)),
            _ => {}
        }
    }
    // 折叠覆盖到末尾（刚压缩完的常态）：标记落在列表最后
    if marker_idx == Some(messages.len()) {
        if let Some(marker) = compaction_marker(session) {
            col = col.push(marker);
        }
    }
    // 覆盖层恒定 Stack 承载、按需追加：显隐不换型（Scrollable ↔ Stack 换型会整树重建、丢滚动位置）
    let flow = scrollable(
        container(col)
            .width(Length::Fill)
            .padding(Padding::ZERO.right(10.0)),
    )
        .id(crate::chat_actions::chat_flow_id())
        .on_scroll(|v| Message::Chat(ChatMsg::FlowScrolled(v.relative_offset().y)))
        .width(Length::Fill)
        .height(Length::Fill);
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

/// 压缩标记的插入位（消息数组下标；= 长度表示落在列表末尾）；无注解 / 锚点失配 = None。
fn compaction_index(session: &Value) -> Option<usize> {
    let compaction = session.get("compaction")?;
    if compaction.is_null() {
        return None;
    }
    let messages = session.get("messages").and_then(Value::as_array)?;
    let anchor = compaction.get("upToMessageId").and_then(Value::as_str)?;
    Some(
        messages
            .iter()
            .position(|m| m.get("id").and_then(Value::as_str) == Some(anchor))
            .map(|i| i + 1)
            .unwrap_or(messages.len()),
    )
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

/// user 气泡（右对齐、按内容收缩、上限 560px、displayContent 优先；引用标签按
/// mention 胶囊渲染，图片附件出缩略图行）。
fn user_bubble<'a>(
    app: &'a App,
    window: iced::window::Id,
    m: &'a Value,
) -> iced::Element<'a, Message> {
    // displayContent 存在即以它为正文（可为空——纯图消息）；缺失（旧消息）回落 content
    let source = match m.get("displayContent") {
        Some(Value::String(s)) => s.as_str(),
        _ => m.get("content").and_then(Value::as_str).unwrap_or(""),
    };
    // 引用标签签名与解析缓存同源（前缀：标签），命中才走 mention 胶囊文档
    let sig = crate::chat_actions::mention_sig(m);
    let mut col = column![].spacing(4);
    // 正文：单行纯文本直接按文本渲染（Markdown 整段渲染恒撑满宽度，短句会得到
    // 一个巨大的空盒子）；带引用的消息用 mention 候选解析（胶囊高亮）
    let body: iced::Element<'a, Message> = if !sig.is_empty() {
        doc_by_key(app, source, crate::chat_actions::doc_key(source, &sig))
    } else if is_plain_line(source) {
        text(source.to_string())
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .into()
    } else {
        markdown_doc(app, source)
    };
    col = col.push(
        container(body)
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
    // 图片附件缩略图行（句柄未载入期间显示占位）
    let thumbs = m
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| a.get("kind").and_then(Value::as_str) == Some("image"))
        .filter_map(|a| a.get("file").and_then(Value::as_str))
        .map(|rel| attachment_thumb(app, rel));
    let thumbs: Vec<_> = thumbs.collect();
    if !thumbs.is_empty() {
        col = col.push(row(thumbs).spacing(6));
    }
    col = col.push(action_row(app, window, m, false));
    container(container(col).align_x(iced::alignment::Horizontal::Right).width(Length::Fill))
        .width(Length::Fill)
        .into()
}

/// 附件缩略图（上限 160px 见方；未载入显示占位块）。
fn attachment_thumb<'a>(app: &'a App, rel: &str) -> iced::Element<'a, Message> {
    let placeholder = || {
        container(icon("image", 20.0, pal::TEXT_MUTED))
            .width(96.0)
            .height(64.0)
            .align_y(iced::alignment::Vertical::Center)
            .align_x(iced::alignment::Horizontal::Center)
            .style(|_| container::Style {
                background: Some(pal::BG_TERTIARY.into()),
                border: border::Border {
                    width: 0.5,
                    radius: pal::RADIUS_SM.into(),
                    color: pal::BORDER,
                },
                ..Default::default()
            })
            .into()
    };
    match app.note_images.get(rel) {
        Some(handle) => iced::widget::image(handle.clone())
            .width(Length::Fixed(160.0))
            .height(Length::Fixed(160.0))
            .filter_method(iced::widget::image::FilterMethod::Linear)
            .into(),
        None => placeholder(),
    }
}

/// assistant 块（无气泡全宽）：步骤块（思考 / 工具可折叠 / 叙述行）+ 最终正文 + 操作。
fn assistant_block<'a>(
    app: &'a App,
    window: iced::window::Id,
    m: &'a Value,
    streaming: bool,
) -> iced::Element<'a, Message> {
    let content = m.get("content").and_then(Value::as_str).unwrap_or("");
    let message_id = m.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let steps = m.get("steps").and_then(Value::as_array);
    let mut col = column![].spacing(6);
    if let Some(steps) = steps {
        let last_step = steps.len().saturating_sub(1);
        for (idx, s) in steps.iter().enumerate() {
            let key = format!("{message_id}:{idx}");
            match s.get("kind").and_then(Value::as_str) {
                Some("reasoning") => {
                    col = col.push(reasoning_block(app, s, streaming && idx == last_step, &key))
                }
                Some("text") => {
                    let t = s.get("text").and_then(Value::as_str).unwrap_or("");
                    if !t.is_empty() {
                        col = col.push(markdown_doc(app, t));
                    }
                }
                Some("tool") => col = col.push(tool_block(app, s, &key)),
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
    col = col.push(action_row(app, window, m, true));
    container(col).width(Length::Fill).into()
}

/// 思考块：折叠态一行摘要（流式取最新一行、完成后取首行）。
fn reasoning_block<'a>(
    app: &'a App,
    s: &'a Value,
    streaming: bool,
    key: &str,
) -> iced::Element<'a, Message> {
    let full = s.get("text").and_then(Value::as_str).unwrap_or("");
    let summary = if streaming {
        full.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").trim()
    } else {
        full.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()
    };
    details_block(
        app,
        key,
        StepBlock {
            leading: vec![icon("brain", 12.0, pal::TEXT_MUTED)],
            title: "思考过程".to_string(),
            summary: summary.to_string(),
            mono_summary: false,
            body: full.to_string(),
            empty_hint: "思考内容为空",
        },
    )
}

/// 工具块：折叠态 = 状态色图示 + 工具名 + 等宽参数/结果摘要；展开态 = 参数与结果分节。
fn tool_block<'a>(app: &'a App, s: &'a Value, key: &str) -> iced::Element<'a, Message> {
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
    let summary = if result_summary.is_empty() {
        args_summary.to_string()
    } else {
        format!("{args_summary} · {result_summary}")
    };
    let (status_icon, status_color) = match status {
        "error" => ("circle-x", pal::DANGER),
        "done" => ("check", pal::SUCCESS),
        _ => ("loader-circle", pal::TEXT_MUTED),
    };
    details_block(
        app,
        key,
        StepBlock {
            leading: vec![
                icon(tool_icon(&name), 12.0, pal::TEXT_MUTED),
                icon(status_icon, 11.0, status_color),
            ],
            title: crate::settings_view::ai::tool_label(&name).to_string(),
            summary,
            mono_summary: true,
            body: detail,
            empty_hint: "",
        },
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

/// 步骤块的展示内容：折叠态（前置图形 + 标题 + 摘要）与展开态正文。
struct StepBlock<'a> {
    leading: Vec<iced::Element<'a, Message>>,
    title: String,
    summary: String,
    /// 摘要是否等宽字体（工具参数 / 结果摘要用）。
    mono_summary: bool,
    body: String,
    empty_hint: &'a str,
}

/// 可折叠步骤块（思考 / 工具共用）：折叠态一行，展开态正文（超高滚动）。
/// 展开态存 `ChatState.expanded`（键 = 消息 id + 步骤序号）。
/// 折叠块摘要单行化：换行折叠为空格 + 按字符数截断加省略号（工具结果天然多行）。
fn one_line(s: &str, cap_chars: usize) -> String {
    let mut out: String = s.chars().filter(|c| !c.is_control()).take(cap_chars + 1).collect();
    if out.chars().count() > cap_chars {
        out = out.chars().take(cap_chars).collect();
        out.push('\u{2026}');
    }
    out
}

fn details_block<'a>(app: &'a App, key: &str, spec: StepBlock<'a>) -> iced::Element<'a, Message> {
    let StepBlock {
        leading,
        title,
        summary,
        mono_summary,
        body,
        empty_hint,
    } = spec;
    let open = app.chat.expanded.contains(key);
    let mut head = row![icon(
        if open { "chevron-down" } else { "chevron-right" },
        12.0,
        pal::TEXT_MUTED
    )]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    for item in leading {
        head = head.push(item);
    }
    head = head.push(text(title).size(pal::fs_micro()).color(pal::TEXT_SECONDARY));
    if !summary.is_empty() {
        // 折叠态头部恒一行：超长摘要视图层截断（展开正文仍完整），防头部被撑成多行
        let mut summary_text =
            text(one_line(&summary, SUMMARY_LINE_CHARS))
                .size(pal::fs_micro())
                .color(pal::TEXT_MUTED);
        if mono_summary {
            summary_text = summary_text.font(crate::layout_view::mono());
        }
        head = head.push(summary_text);
    }
    let header = button(container(head).width(Length::Fill))
        .width(Length::Fill)
        .padding(Padding::ZERO.vertical(5.0))
        .style(controls::row_style(false, pal::RADIUS_XS))
        .on_press(Message::Chat(ChatMsg::ToggleStep(key.to_string())));
    let mut col = column![header];
    if open {
        if !body.is_empty() {
            col = col.push(controls::h_divider_subtle());
            col = col.push(
                scrollable(
                    container(text(body.to_string()).size(pal::fs_micro()).color(pal::TEXT_SECONDARY))
                        .width(Length::Fill)
                        .padding(Padding::ZERO.vertical(6.0)),
                )
                .height(160.0),
            );
        } else if !empty_hint.is_empty() {
            col = col.push(
                container(text(empty_hint.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED))
                    .padding(Padding::ZERO.vertical(6.0)),
            );
        }
    }
    // 卡片与回复正文同宽同位：正文已无根留白，卡片无需额外内缩（消息列边距即两者共同边界）
    container(col)
        .width(Length::Fill)
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
fn action_row<'a>(
    app: &'a App,
    window: iced::window::Id,
    m: &'a Value,
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
    app.chat
        .sessions
        .iter()
        .find(|s| s["id"] == json!(session_id))
        .and_then(|s| s.get("messages").and_then(Value::as_array))
        .and_then(|ms| ms.last())
        .map(|last| last.get("id").and_then(Value::as_str) == m.get("id").and_then(Value::as_str))
        .unwrap_or(false)
}

// ===== 输入区 =====

/// 自绘输入面事件的 Elm 路由。
fn chat_input_msg(event: crate::chat_input::InputEvent) -> Message {
    Message::Chat(ChatMsg::Input(event))
}

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
    let tool_row = row![
        plus_btn,
        agent_trigger(app, window),
        model_select_trigger(app, window),
        Space::new().width(Length::Fill),
        send_btn,
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);

    // 自绘输入面（# 标签高亮 / 整标签交互由部件托管）
    let surface = crate::chat_input::Input::new(
        &app.chat.input,
        &app.chat.mention_refs,
        chat_input_msg,
    )
    .picker_open(app.chat.mention_picker.is_some())
    .placeholder("输入消息，Enter 发送，Shift+Enter 换行");
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
    let rows = picker.candidates.iter().enumerate().map(|(i, (rel, is_dir))| {
        let selected = i == picker.selected;
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
        scrollable(column(rows.map(iced::Element::from)).spacing(0).width(Length::Fill))
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
    container(scrollable(row(chips.map(iced::Element::from)).spacing(6)))
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

/// 对话消息 markdown 渲染（复用阅读面管线；解析缓存由 ChatState 维护，
/// 未命中回落纯文本）。图片暂不参与（对话消息内嵌图片走后续批次）。
fn markdown_doc<'a>(app: &'a App, source: &'a str) -> iced::Element<'a, Message> {
    doc_by_key(app, source, crate::chat_actions::source_hash(source))
}

/// 按缓存键取解析文档渲染（键由调用方决定：普通 = 源哈希；带引用 = 源 + 引用签名）。
fn doc_by_key<'a>(app: &'a App, source: &'a str, key: u64) -> iced::Element<'a, Message> {
    static EMPTY_IMAGES: std::sync::OnceLock<crate::markdown_view::ImageHandles> =
        std::sync::OnceLock::new();
    let images = EMPTY_IMAGES.get_or_init(std::collections::HashMap::new);
    match app.chat.docs.get(&key) {
        Some(doc) => crate::markdown_view::document_body(doc, &app.theme, images, true),
        None => text(source.to_string())
            .size(pal::fs_body())
            .color(pal::TEXT_PRIMARY)
            .into(),
    }
}
