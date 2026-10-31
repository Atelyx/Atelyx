//! 面板整面帧：侧栏、消息流、输入区与弹出菜单。
//! 只读模型状态并声明控件；事件由入口按命中 id 解释。

use serde_json::Value;

use atelyx_plugin_protocol::paint::{ColorToken, ImageFit, ImageSource};
use atelyx_plugin_sdk::frame_ui::{
    BoxStyle, BoxWidth, Edges, Rect, TextRole, TextStyle, Ui,
};

use crate::model::{
    content_of, display_of, message_id_of, role_of, Candidate, Menu, Model, FLOW,
};

/// 侧栏宽度与并排显示的最小面板宽。
const SIDEBAR_W: f32 = 228.0;
const SIDEBAR_INLINE_MIN_W: f32 = 640.0;
/// 输入盒内文本区高度与图片缩略图边长。
const INPUT_H: f32 = 72.0;
const THUMB: f32 = 160.0;
/// 浮层宽度与边距。
const MENU_PLUS_W: f32 = 176.0;
const MENU_AGENT_W: f32 = 200.0;
const MENU_MODEL_W: f32 = 224.0;
const MENU_EFFORT_W: f32 = 224.0;
const MENU_ROW_H: f32 = 28.0;
const MENU_PAD: f32 = 4.0;
/// 折叠块摘要单行字符上限。
const SUMMARY_CHARS: usize = 56;

pub fn frame(ui: &mut Ui<'_, '_>, model: &Model) {
    let bounds = ui.bounds();
    let narrow = bounds.w < SIDEBAR_INLINE_MIN_W;
    let chat_width = if model.sidebar_open && !narrow {
        (bounds.w - SIDEBAR_W - 1.0).max(1.0)
    } else {
        bounds.w
    };
    let overlay_sidebar = model.sidebar_open && narrow;
    if overlay_sidebar {
        // 背板先声明：主内容与侧栏的命中都压在它上面，只有真空白处才收起侧栏
        ui.stack(|stack| {
            stack.hit_area("sidebar-backdrop");
            stack.fill_column(0.0, |root| {
                root.row_fill(0.0, |row| chat_column(row, model, chat_width));
            });
        });
        ui.overlay(Rect::new(0.0, 0.0, SIDEBAR_W, bounds.h), |layer| {
            layer.side(SIDEBAR_W, |side| sidebar_body(side, model));
        });
    } else {
        ui.fill_column(0.0, |root| {
            root.row_fill(0.0, |row| {
                if model.sidebar_open {
                    sidebar(row, model, SIDEBAR_W);
                    row.bar(1.0, ColorToken::Border);
                }
                chat_column(row, model, chat_width);
            });
        });
    }
    let left = if model.sidebar_open && !narrow { SIDEBAR_W + 1.0 } else { 0.0 };
    if model.menu.is_some() || model.picker.is_some() {
        popup_layer(ui, model, bounds, left);
    }
    if !model.follow_bottom && !model.messages.is_empty() {
        let width = (bounds.w - left).max(1.0);
        let rect = Rect::new(left + (width - 96.0) / 2.0, (bounds.h - 152.0).max(0.0), 96.0, 28.0);
        ui.overlay(rect, |layer| {
            layer.click_box(
                "scroll-bottom",
                BoxWidth::Fill,
                Edges::symmetric(10.0, 5.0),
                999.0,
                false,
                true,
                |row| {
                    row.row(4.0, |line| {
                        line.icon_named("chevron-down", 11.0, ColorToken::TextSecondary);
                        line.text_styled(
                            "回到底部",
                            TextStyle::new(TextRole::Micro).color(ColorToken::TextSecondary),
                        );
                    });
                },
            );
        });
    }
}

fn sidebar(ui: &mut Ui<'_, '_>, model: &Model, width: f32) {
    ui.side(width, |side| sidebar_body(side, model));
}

fn sidebar_body(ui: &mut Ui<'_, '_>, model: &Model) {
    let theme_bg = BoxStyle::new(Some(ColorToken::SurfaceMuted), None, 0.0);
    ui.boxed(BoxWidth::Fill, theme_bg, Edges::all(0.0), 0.0, |panel| {
        panel.stack(|stack| {
            // 侧栏面板整体吞掉按压，避免落到背板上把侧栏收起
            stack.hit_area("sidebar-swallow");
            stack.pad(6.0, 6.0, |body| {
                body.text_input("search", model.search.clone(), "搜索会话");
                body.spacer(6.0);
                body.scroll("sessions", 2.0, |list| {
                    for session in model.visible_sessions() {
                        session_row(list, model, session);
                    }
                    if model.sessions.is_empty() {
                        list.pad(8.0, 8.0, |empty| {
                            let label = if model.search.trim().is_empty() {
                                "暂无历史会话"
                            } else {
                                "没有匹配的会话"
                            };
                            empty.caption(label.to_string());
                        });
                    }
                });
            });
        });
    });
}

fn session_row(ui: &mut Ui<'_, '_>, model: &Model, session: &crate::model::SessionRow) {
    let active = model.active.as_deref() == Some(session.id.as_str());
    let menu_open = model.session_menu.as_deref() == Some(session.id.as_str());
    let confirm = model.delete_confirm.as_deref() == Some(session.id.as_str());
    let row_id = format!("session:{}", session.id);
    let hovered = ui.hovered(&row_id);
    ui.click_row(
        row_id,
        Edges { left: 0.0, right: 6.0, top: 6.0, bottom: 6.0 },
        6.0,
        active,
        true,
        |row| {
            row.row(6.0, |line| {
                line.bar(2.0, if active { ColorToken::Accent } else { ColorToken::Transparent });
                line.column(1.0, |text| {
                    text.text_styled(
                        session.title().to_string(),
                        TextStyle::new(TextRole::Caption)
                            .color(if active { ColorToken::Accent } else { ColorToken::Text }),
                    );
                    text.text_styled(
                        rel_time(session.updated_at),
                        TextStyle::new(TextRole::Micro),
                    );
                });
                if confirm {
                    line.click_box(
                        format!("confirm:{}", session.id),
                        BoxWidth::Fixed(72.0),
                        Edges::all(4.0),
                        6.0,
                        false,
                        true,
                        |button| {
                            button.text_styled(
                                "确认删除？",
                                TextStyle::new(TextRole::Micro).color(ColorToken::Danger),
                            );
                        },
                    );
                    line.icon_button_named(
                        format!("cancel-delete:{}", session.id),
                        "x",
                        12.0,
                        20.0,
                        ColorToken::TextMuted,
                        true,
                    );
                } else if menu_open {
                    line.icon_button_named(
                        format!("delete:{}", session.id),
                        "trash-2",
                        12.0,
                        20.0,
                        ColorToken::TextMuted,
                        true,
                    );
                    line.icon_button_named(
                        format!("close-menu:{}", session.id),
                        "x",
                        12.0,
                        20.0,
                        ColorToken::TextMuted,
                        true,
                    );
                } else if hovered {
                    line.icon_button_named(
                        format!("more:{}", session.id),
                        "more-horizontal",
                        12.0,
                        20.0,
                        ColorToken::TextMuted,
                        true,
                    );
                }
            });
        },
    );
}

fn chat_column(ui: &mut Ui<'_, '_>, model: &Model, chat_width: f32) {
    ui.fill_column(0.0, |column| {
        top_bar(column, model);
        column.rule();
        message_flow(column, model, chat_width);
        column.rule();
        composer(column, model, chat_width);
    });
}

fn top_bar(ui: &mut Ui<'_, '_>, model: &Model) {
    let error = model.error.clone().or_else(|| model.persist_error.clone());
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::SurfaceMuted), None, 0.0),
        Edges::symmetric(10.0, 6.0),
        6.0,
        |bar| {
            bar.row(6.0, |line| {
                if let Some(error) = &error {
                    line.icon_named("circle-x", 13.0, ColorToken::Danger);
                    line.text_styled(
                        error.clone(),
                        TextStyle::new(TextRole::Caption).color(ColorToken::Danger),
                    );
                } else {
                    line.icon_named("message-square", 13.0, ColorToken::TextSecondary);
                    line.text_styled(
                        model.title(),
                        TextStyle::new(TextRole::Caption).color(ColorToken::Text),
                    );
                }
                if model.active.is_some() && !model.active_streaming() {
                    if model.active_renaming() {
                        line.icon_named("loader-circle", 14.0, ColorToken::Accent);
                    } else {
                        line.icon_button_named(
                            "regen-title",
                            "refresh-cw",
                            12.0,
                            22.0,
                            ColorToken::TextSecondary,
                            true,
                        );
                    }
                }
                line.grow();
                line.icon_button_named(
                    "new",
                    "file-plus",
                    13.0,
                    24.0,
                    ColorToken::TextSecondary,
                    true,
                );
                line.icon_button_named(
                    "toggle-sidebar",
                    "history",
                    13.0,
                    24.0,
                    ColorToken::TextSecondary,
                    true,
                );
                if model.active.is_some() && model.messages.len() >= 2 {
                    if model.active_compacting() {
                        line.icon_button_named(
                            "compact",
                            "loader-circle",
                            13.0,
                            24.0,
                            ColorToken::TextMuted,
                            false,
                        );
                    } else {
                        line.icon_button_named(
                            "compact",
                            "layers",
                            13.0,
                            24.0,
                            ColorToken::TextSecondary,
                            true,
                        );
                    }
                }
                if error.is_some() {
                    line.icon_button_named(
                        "clear-error",
                        "x",
                        13.0,
                        24.0,
                        ColorToken::TextSecondary,
                        true,
                    );
                }
            });
        },
    );
}

fn message_flow(ui: &mut Ui<'_, '_>, model: &Model, chat_width: f32) {
    let marker = compaction_index(model);
    let len = model.messages.len() + usize::from(marker.is_some());
    if len == 0 {
        empty_flow(ui);
        return;
    }
    let viewport = (ui.bounds().h - 180.0).max(160.0);
    ui.pad(12.0, 0.0, |flow| {
        flow.spacer(12.0);
        flow.virtual_scroll(
            FLOW,
            len,
            10.0,
            true,
            viewport,
            120.0,
            |index| match marker {
                Some(marker) if index == marker => u64::MAX,
                Some(marker) if index > marker => message_key(model, index - 1),
                _ => message_key(model, index),
            },
            |item, index| match marker {
                Some(marker) if index == marker => compaction_marker(item, model),
                Some(marker) if index > marker => {
                    message_block(item, model, &model.messages[index - 1], chat_width)
                }
                _ => message_block(item, model, &model.messages[index], chat_width),
            },
        );
        flow.spacer(12.0);
    });
}

fn empty_flow(ui: &mut Ui<'_, '_>) {
    ui.column(8.0, |outer| {
        outer.center_column(8.0, |center| {
            center.boxed(
                BoxWidth::Fixed(40.0),
                BoxStyle::new(Some(ColorToken::Surface), None, 10.0),
                Edges::all(0.0),
                0.0,
                |icon| {
                    icon.spacer(11.0);
                    icon.row(0.0, |line| {
                        line.grow();
                        line.icon_named("message-square", 18.0, ColorToken::TextMuted);
                        line.grow();
                    });
                    icon.spacer(11.0);
                },
            );
            center.text_styled(
                "开始新的 AI 对话",
                TextStyle::new(TextRole::Heading).color(ColorToken::Text),
            );
        });
    });
}

fn compaction_index(model: &Model) -> Option<usize> {
    let compaction = model.compaction.as_ref()?;
    if compaction.is_null() {
        return None;
    }
    let anchor = compaction.get("upToMessageId").and_then(Value::as_str)?;
    Some(
        model
            .messages
            .iter()
            .position(|message| message_id_of(message) == anchor)
            .map(|index| index + 1)
            .unwrap_or(model.messages.len()),
    )
}

fn compaction_marker(ui: &mut Ui<'_, '_>, model: &Model) {
    let Some(compaction) = &model.compaction else { return };
    let summary = compaction.get("summary").and_then(Value::as_str).unwrap_or("");
    let count = compaction.get("messageCount").and_then(Value::as_u64).unwrap_or(0);
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, Some(ColorToken::Border), 10.0).hairline(),
        Edges::all(8.0),
        4.0,
        |card| {
            card.text_styled(
                format!("上下文已压缩 · 以上 {count} 条已折叠为摘要"),
                TextStyle::new(TextRole::Micro),
            );
            if !summary.is_empty() {
                card.text_styled(
                    summary.to_string(),
                    TextStyle::new(TextRole::Micro)
                        .color(ColorToken::TextSecondary)
                        .wrap(),
                );
            }
        },
    );
}

fn message_block(ui: &mut Ui<'_, '_>, model: &Model, message: &Value, chat_width: f32) {
    let id = message_id_of(message).to_string();
    let assistant = role_of(message) == "assistant";
    if assistant {
        assistant_block(ui, model, message, &id);
    } else {
        user_block(ui, model, message, &id, chat_width);
    }
}

fn user_block(ui: &mut Ui<'_, '_>, model: &Model, message: &Value, id: &str, chat_width: f32) {
    let display = display_of(message);
    let plain = is_plain_line(display);
    let max_width = 560.0_f32.min((chat_width - 24.0).max(80.0));
    let bubble_width = if plain {
        (ui.text_width(display, TextRole::Body) + 20.0).min(max_width)
    } else {
        max_width
    };
    ui.column(4.0, |column| {
        if !display.is_empty() {
            column.row(0.0, |line| {
                line.grow();
                line.boxed(
                    BoxWidth::Fixed(bubble_width.max(1.0)),
                    BoxStyle::new(Some(ColorToken::Surface), Some(ColorToken::Border), 10.0)
                        .hairline(),
                    Edges::all(10.0),
                    0.0,
                    |bubble| {
                        if plain {
                            bubble.text_styled(
                                display.to_string(),
                                TextStyle::new(TextRole::Body).wrap(),
                            );
                        } else {
                            bubble.markdown(display.to_string());
                        }
                    },
                );
            });
        }
        if let Some(attachments) = message.get("attachments").and_then(Value::as_array) {
            let images: Vec<&str> = attachments
                .iter()
                .filter(|attachment| {
                    attachment.get("kind").and_then(Value::as_str) == Some("image")
                })
                .filter_map(|attachment| attachment.get("file").and_then(Value::as_str))
                .collect();
            if !images.is_empty() {
                column.row(6.0, |line| {
                    line.grow();
                    for file in images {
                        line.image_sized(
                            ImageSource::Vault { path: file.to_string() },
                            THUMB,
                            THUMB,
                            Some(ImageFit::Contain),
                        );
                    }
                });
            }
        }
        column.row(0.0, |line| {
            line.grow();
            line.row_fit(2.0, |actions| {
                action_items(actions, model, message, false, id);
            });
        });
    });
}

fn assistant_block(ui: &mut Ui<'_, '_>, model: &Model, message: &Value, id: &str) {
    let content = content_of(message);
    let steps = message.get("steps").and_then(Value::as_array);
    let streaming = model.active_streaming() && model.is_last(id);
    let mut has_tools = false;
    ui.column(6.0, |column| {
        if let Some(steps) = steps {
            let last = steps.len().saturating_sub(1);
            for (index, step) in steps.iter().enumerate() {
                let key = format!("{id}:{index}");
                match step.get("kind").and_then(Value::as_str) {
                    Some("reasoning") => reasoning_block(
                        column,
                        model,
                        step,
                        streaming && index == last,
                        &key,
                    ),
                    Some("text") => {
                        if let Some(text) = step.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                column.markdown(text.to_string());
                            }
                        }
                    }
                    Some("tool") => {
                        has_tools = true;
                        tool_block(column, model, step, &key);
                    }
                    _ => {}
                }
            }
        }
        if has_tools && !content.is_empty() {
            column.rule();
        }
        if !content.is_empty() {
            column.markdown(content.to_string());
        } else if streaming && steps.map(|steps| steps.is_empty()).unwrap_or(true) {
            column.text_styled("生成中…", TextStyle::new(TextRole::Caption));
        }
        action_row(column, model, message, true, id);
    });
}

fn reasoning_block(
    ui: &mut Ui<'_, '_>,
    model: &Model,
    step: &Value,
    streaming: bool,
    key: &str,
) {
    let full = step.get("text").and_then(Value::as_str).unwrap_or("");
    let summary = if streaming {
        full.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("").trim()
    } else {
        full.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim()
    };
    details_block(
        ui,
        model,
        Details {
            key,
            leading: vec![("brain", ColorToken::TextMuted)],
            title: "思考过程".to_string(),
            summary: summary.to_string(),
            mono_summary: false,
            body: if model.expanded.contains(key) {
                full.to_string()
            } else {
                String::new()
            },
            empty_hint: "思考内容为空",
        },
    );
}

fn tool_block(ui: &mut Ui<'_, '_>, model: &Model, step: &Value, key: &str) {
    let run = step.get("run").cloned().unwrap_or(Value::Null);
    let name = run.get("name").and_then(Value::as_str).unwrap_or("tool");
    let status = run.get("status").and_then(Value::as_str).unwrap_or("running");
    let args_summary = run.get("argsSummary").and_then(Value::as_str).unwrap_or("");
    let result_summary = run.get("resultSummary").and_then(Value::as_str).unwrap_or("");
    // 参数与结果是任意长的工具输出：只有卡片展开时才格式化
    let detail = if model.expanded.contains(key) {
        let args_json = run.get("args").map(pretty_json).unwrap_or_default();
        let result = run.get("result").and_then(Value::as_str).unwrap_or("");
        format!(
            "参数\n{}\n\n结果\n{}",
            if args_json.is_empty() { "—" } else { args_json.as_str() },
            if status == "running" {
                "进行中…".to_string()
            } else if result.is_empty() {
                result_summary.to_string()
            } else {
                result.to_string()
            }
        )
    } else {
        String::new()
    };
    let summary = if result_summary.is_empty() {
        args_summary.to_string()
    } else {
        format!("{args_summary} · {result_summary}")
    };
    let status_icon = match status {
        "error" => ("circle-x", ColorToken::Danger),
        "done" => ("check", ColorToken::Success),
        _ => ("loader-circle", ColorToken::TextMuted),
    };
    details_block(
        ui,
        model,
        Details {
            key,
            leading: vec![(tool_icon(name), ColorToken::TextMuted), status_icon],
            title: tool_label(name),
            summary,
            mono_summary: true,
            body: detail,
            empty_hint: "",
        },
    );
}

struct Details<'a> {
    key: &'a str,
    leading: Vec<(&'a str, ColorToken)>,
    title: String,
    summary: String,
    mono_summary: bool,
    body: String,
    empty_hint: &'a str,
}

fn details_block(ui: &mut Ui<'_, '_>, model: &Model, spec: Details<'_>) {
    let open = model.expanded.contains(spec.key);
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::SurfaceSunken), Some(ColorToken::Border), 6.0).hairline(),
        Edges { left: 0.0, right: 0.0, top: 6.0, bottom: 6.0 },
        0.0,
        |card| {
            card.click_row(
                format!("step:{}", spec.key),
                Edges::symmetric(0.0, 5.0),
                4.0,
                false,
                true,
                |head| {
                    head.row(6.0, |line| {
                        line.icon_named(
                            if open { "chevron-down" } else { "chevron-right" },
                            12.0,
                            ColorToken::TextMuted,
                        );
                        for (name, color) in &spec.leading {
                            line.icon_named(name, 12.0, *color);
                        }
                        line.text_styled_fit(
                            spec.title.clone(),
                            TextStyle::new(TextRole::Micro).color(ColorToken::TextSecondary),
                        );
                        if !spec.summary.is_empty() {
                            let mut style = TextStyle::new(TextRole::Micro);
                            if spec.mono_summary {
                                style = style.mono();
                            }
                            line.text_styled_fit(one_line(&spec.summary, SUMMARY_CHARS), style);
                        }
                    });
                },
            );
            if open {
                if !spec.body.is_empty() {
                    card.rule();
                    card.pad(0.0, 6.0, |body| {
                        body.scroll_sized(format!("step-body:{}", spec.key), 160.0, 0.0, false, |scroll| {
                            scroll.text_styled(
                                spec.body.clone(),
                                TextStyle::new(TextRole::Micro)
                                    .color(ColorToken::TextSecondary)
                                    .wrap(),
                            );
                        });
                    });
                } else if !spec.empty_hint.is_empty() {
                    card.pad(0.0, 6.0, |hint| {
                        hint.text_styled(spec.empty_hint, TextStyle::new(TextRole::Micro));
                    });
                }
            }
        },
    );
}

fn action_row(
    ui: &mut Ui<'_, '_>,
    model: &Model,
    message: &Value,
    assistant: bool,
    id: &str,
) {
    ui.row(2.0, |line| action_items(line, model, message, assistant, id));
}

fn action_items(
    ui: &mut Ui<'_, '_>,
    model: &Model,
    message: &Value,
    assistant: bool,
    id: &str,
) {
    let display = display_of(message);
    let content = content_of(message);
    {
        let line = ui;
        if !display.is_empty() {
            line.icon_button_named(
                format!("copy:{id}"),
                "copy",
                12.0,
                22.0,
                ColorToken::TextSecondary,
                true,
            );
        }
        if assistant && !content.is_empty() && !content.starts_with("[错误]") {
            line.icon_button_named(
                format!("rollback:{id}"),
                "corner-up-left",
                12.0,
                22.0,
                ColorToken::TextSecondary,
                true,
            );
            let actionable = model.is_last(id) && !model.active_streaming();
            line.icon_button_named(
                format!("regen:{id}"),
                "refresh-cw",
                12.0,
                22.0,
                ColorToken::TextSecondary,
                actionable,
            );
        }
        if model.rollback_confirm.as_deref() == Some(id) {
            line.click_box(
                format!("rollback-confirm:{id}"),
                BoxWidth::Fixed(64.0),
                Edges::all(4.0),
                4.0,
                false,
                true,
                |button| {
                    button.text_styled(
                        "确认回滚？",
                        TextStyle::new(TextRole::Micro).color(ColorToken::Danger),
                    );
                },
            );
            line.click_box(
                "rollback-cancel",
                BoxWidth::Fixed(36.0),
                Edges::all(4.0),
                4.0,
                false,
                true,
                |button| {
                    button.text_styled("取消", TextStyle::new(TextRole::Micro));
                },
            );
        }
    }
}

fn composer(ui: &mut Ui<'_, '_>, model: &Model, chat_width: f32) {
    ui.pad(12.0, 0.0, |area| {
        if !model.images.is_empty() {
            area.spacer(8.0);
            image_tray(area, model);
        }
        area.spacer(8.0);
        area.boxed(
            BoxWidth::Fill,
            BoxStyle::new(Some(ColorToken::SurfaceSunken), Some(ColorToken::Border), 10.0)
                .hairline(),
            Edges::all(8.0),
            0.0,
            |input| {
                input.text_area_submit_plain(
                    crate::model::DRAFT,
                    model.draft.clone(),
                    "输入消息，Enter 发送，Shift+Enter 换行",
                    INPUT_H,
                    14.0,
                    Edges { left: 2.0, right: 2.0, top: 8.0, bottom: 0.0 },
                );
                input.spacer(4.0);
                tool_row(input, model, chat_width);
            },
        );
        area.spacer(10.0);
    });
}

fn tool_row(ui: &mut Ui<'_, '_>, model: &Model, chat_width: f32) {
    ui.row(6.0, |line| {
        let plus_open = model.menu == Some(Menu::Plus);
        line.icon_button_named(
            "plus-menu",
            "plus",
            13.0,
            24.0,
            if plus_open { ColorToken::Accent } else { ColorToken::TextSecondary },
            true,
        );
        trigger(line, "agent-menu", "bot", model.agent_label(), chat_width);
        trigger(
            line,
            "model-menu",
            "sparkles",
            format!("{} · {}", model.model_label(), model.effort_label()),
            chat_width,
        );
        line.grow();
        if model.active_streaming() {
            line.icon_button_filled(
                "stop",
                "square",
                14.0,
                28.0,
                ColorToken::Surface,
                ColorToken::Text,
                true,
            );
        } else {
            line.icon_button_filled_color(
                "send",
                "arrow-up",
                14.0,
                28.0,
                ColorToken::Accent,
                atelyx_plugin_protocol::paint::PaintColor::Rgba { rgba: [0x14, 0x14, 0x14, 255] },
                true,
            );
        }
    });
}

fn trigger(ui: &mut Ui<'_, '_>, id: &str, icon: &str, label: String, chat_width: f32) {
    let width = (12.0 + 4.0 + ui.text_width(&label, TextRole::Micro) + 4.0 + 10.0 + 12.0)
        .min(((chat_width - 100.0) / 2.0).max(80.0));
    ui.click_box(
        id,
        BoxWidth::Fixed(width),
        Edges::symmetric(6.0, 3.0),
        4.0,
        false,
        true,
        |row| {
            row.row(4.0, |line| {
                line.icon_named(icon, 12.0, ColorToken::TextSecondary);
                line.text_styled(label.clone(), TextStyle::new(TextRole::Micro));
                line.icon_named("chevron-down", 10.0, ColorToken::TextMuted);
            });
        },
    );
}

fn image_tray(ui: &mut Ui<'_, '_>, model: &Model) {
    let chips: Vec<(usize, String, f32)> = model
        .images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let width = (12.0 + 4.0 + ui.text_width(&image.name, TextRole::Micro) + 4.0 + 10.0 + 16.0)
                .min(200.0);
            (index, image.name.clone(), width)
        })
        .collect();
    ui.row(6.0, |line| {
        for (index, name, width) in chips {
            line.click_box(
                format!("remove-image:{index}"),
                BoxWidth::Fixed(width),
                Edges::symmetric(8.0, 3.0),
                4.0,
                false,
                true,
                |chip| {
                    chip.row(4.0, |row| {
                        row.icon_named("image", 12.0, ColorToken::TextSecondary);
                        row.text_styled(name.clone(), TextStyle::new(TextRole::Micro));
                        row.icon_named("x", 10.0, ColorToken::TextMuted);
                    });
                },
            );
        }
    });
}

fn popup_layer(ui: &mut Ui<'_, '_>, model: &Model, bounds: Rect, left: f32) {
    let available = (bounds.w - left - 24.0).max(120.0);
    let (width, rows) = if let Some(picker) = &model.picker {
        (MENU_MODEL_W.min(available), picker.candidates.len().max(1))
    } else {
        match model.menu {
            Some(Menu::Plus) => (MENU_PLUS_W.min(available), 2),
            Some(Menu::Agent) => (MENU_AGENT_W.min(available), model.agents.len().max(1)),
            Some(Menu::ModelRoot) => (MENU_MODEL_W.min(available), 2),
            Some(Menu::ModelList) => (MENU_MODEL_W.min(available), model.models.len().max(1)),
            Some(Menu::Effort) => (MENU_EFFORT_W.min(available), 5),
            None => (0.0, 0),
        }
    };
    if width <= 0.0 {
        return;
    }
    let max_height = (bounds.h - 180.0).max(60.0);
    let row_h = if model.picker.is_some() { 44.0 } else { MENU_ROW_H };
    let height = (rows as f32 * row_h + MENU_PAD * 2.0).min(max_height);
    let x = (left + 12.0).min((bounds.w - width).max(0.0));
    let tray = if model.images.is_empty() { 0.0 } else { 32.0 };
    let y = (bounds.h - 140.0 - tray - height).max(0.0);
    ui.overlay(Rect::new(x, y, width, height), |panel| {
        panel.boxed(
            BoxWidth::Fill,
            BoxStyle::new(Some(ColorToken::SurfaceMuted), Some(ColorToken::Border), 10.0)
                .hairline(),
            Edges::all(MENU_PAD),
            0.0,
            |menu| {
                if let Some(picker) = &model.picker {
                    picker_box(
                        menu,
                        picker.candidates.as_slice(),
                        picker.selected,
                        height - MENU_PAD * 2.0,
                    );
                } else if let Some(menu_kind) = model.menu {
                    menu_box(menu, model, menu_kind, height - MENU_PAD * 2.0);
                }
            },
        );
    });
}

fn picker_box(ui: &mut Ui<'_, '_>, candidates: &[Candidate], selected: usize, height: f32) {
    ui.scroll_sized("picker", height.max(40.0), 0.0, false, |list| {
        for (index, candidate) in candidates.iter().enumerate() {
            let (icon, label) = if candidate.is_dir {
                ("folder", format!("{}/", candidate.name))
            } else {
                (icon_of_path(&candidate.path), candidate.name.clone())
            };
            list.click_box(
                format!("pick:{index}"),
                BoxWidth::Fill,
                Edges::symmetric(10.0, 5.0),
                6.0,
                index == selected,
                true,
                |row| {
                    row.column(1.0, |lines| {
                        lines.row(6.0, |line| {
                            line.icon_named(icon, 12.0, ColorToken::TextSecondary);
                            line.text_styled(
                                label.clone(),
                                TextStyle::new(TextRole::Caption).color(ColorToken::Text),
                            );
                        });
                        if !candidate.sub.is_empty() {
                            lines.pad(18.0, 0.0, |sub| {
                                sub.text_styled(
                                    candidate.sub.clone(),
                                    TextStyle::new(TextRole::Micro),
                                );
                            });
                        }
                    });
                },
            );
        }
    });
}

fn menu_box(ui: &mut Ui<'_, '_>, model: &Model, menu: Menu, height: f32) {
    match menu {
        Menu::Plus => {
            menu_row(ui, "plus-context", "添加上下文", false, false);
            menu_row(ui, "plus-attach", "上传附件", false, false);
        }
        Menu::Agent => {
            let current = model.active_agent();
            ui.scroll_sized("menu", height, 0.0, false, |list| {
                for agent in &model.agents {
                    let checked = current.as_deref() == Some(agent.id.as_str())
                        || (current.is_none() && agent.name == "对话");
                    menu_row(
                        list,
                        &format!("agent:{}", agent.id),
                        &agent.name,
                        checked,
                        false,
                    );
                }
                if model.agents.is_empty() {
                    list.pad(10.0, 6.0, |empty| empty.caption("Agent 名册尚未读取"));
                }
            });
        }
        Menu::ModelRoot => {
            menu_row(
                ui,
                "open-model",
                &format!("模型：{}", model.model_label()),
                false,
                true,
            );
            menu_row(
                ui,
                "open-effort",
                &format!("推理等级：{}", model.effort_label()),
                false,
                true,
            );
        }
        Menu::ModelList => {
            ui.scroll_sized("menu", height, 0.0, false, |list| {
                let mut provider = "";
                for (index, row) in model.models.iter().enumerate() {
                    if provider != row.provider_name.as_str() {
                        provider = row.provider_name.as_str();
                        list.pad(10.0, 4.0, |header| header.caption(provider.to_string()));
                    }
                    let selected = model
                        .model_pick
                        .as_ref()
                        .is_some_and(|(p, m)| p == &row.provider_id && m == &row.model);
                    menu_row(
                        list,
                        &format!("model:{index}"),
                        &row.label,
                        selected,
                        false,
                    );
                }
                if model.models.is_empty() {
                    list.pad(10.0, 6.0, |empty| empty.caption("供应商未配置模型"));
                }
            });
        }
        Menu::Effort => {
            for (wire, label) in [
                ("default", "默认"),
                ("off", "关闭"),
                ("low", "低"),
                ("medium", "中"),
                ("high", "高"),
            ] {
                let selected = match wire {
                    "default" => model.effort.is_none(),
                    other => model.effort.as_deref() == Some(other),
                };
                menu_row(
                    ui,
                    &format!("effort:{wire}"),
                    label,
                    selected,
                    false,
                );
            }
        }
    }
}

fn menu_row(ui: &mut Ui<'_, '_>, id: &str, label: &str, checked: bool, submenu: bool) {
    ui.click_box(
        id,
        BoxWidth::Fill,
        Edges::symmetric(10.0, 5.5),
        6.0,
        false,
        true,
        |row| {
            row.row(8.0, |line| {
                if checked {
                    line.icon_named("check", 12.0, ColorToken::Accent);
                } else {
                    line.spacer_width(12.0);
                }
                line.text_styled(
                    label.to_string(),
                    TextStyle::new(TextRole::Label).color(if checked {
                        ColorToken::Accent
                    } else {
                        ColorToken::Text
                    }),
                );
                if submenu {
                    line.grow();
                    line.icon_named("chevron-right", 12.0, ColorToken::TextMuted);
                }
            });
        },
    );
}

fn is_plain_line(source: &str) -> bool {
    !source.contains('\n')
        && source.chars().count() <= 200
        && !source.contains(['*', '_', '`', '[', ']', '#', '>', '|', '\\', '~'])
}

fn one_line(source: &str, cap: usize) -> String {
    let mut out: String = source.chars().filter(|ch| !ch.is_control()).take(cap + 1).collect();
    if out.chars().count() > cap {
        out = out.chars().take(cap).collect();
        out.push('\u{2026}');
    }
    out
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
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

fn tool_label(id: &str) -> String {
    match id {
        "web_search" => "联网搜索",
        "web_fetch" => "抓取网页",
        "read_file" => "读取文件",
        "glob" => "查找文件",
        "grep" => "搜索内容",
        "list_dir" => "列出目录",
        "read_history" => "读取历史",
        "edit_file" => "编辑文件",
        "append_file" => "追加内容",
        "write_file" => "写入文件",
        "rename_file" => "重命名文件",
        "move_file" => "移动文件",
        "delete_file" => "删除文件",
        "delete_dir" => "删除目录",
        "todo_write" => "任务清单",
        other => other,
    }
    .to_string()
}

fn icon_of_path(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("atb") => "table",
        Some("atlx") => "layers",
        _ => "file-text",
    }
}

fn message_key(model: &Model, index: usize) -> u64 {
    let id = message_id_of(&model.messages[index]);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn rel_time(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let minutes = (now - timestamp).max(0) / 60_000;
    match minutes {
        0 => "刚刚".to_string(),
        value if value < 60 => format!("{value} 分钟前"),
        value if value < 60 * 24 => format!("{} 小时前", value / 60),
        value if value < 60 * 24 * 30 => format!("{} 天前", value / (60 * 24)),
        _ => "更早".to_string(),
    }
}
