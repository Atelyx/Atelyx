//! 面板整面帧：侧栏、消息流、输入区与弹出菜单。
//! 只读模型状态并声明控件；事件由入口按命中 id 解释。

use serde_json::Value;

use atelyx_plugin_protocol::paint::{
    ColorToken, ImageFit, ImageSource, PaintColor, PaintShadow, TextWeight,
};
use atelyx_plugin_sdk::frame_ui::{
    BoxStyle, BoxWidth, Edges, MentionCandidate, Rect, TextRole, TextStyle, Ui,
};

use crate::model::{
    content_of, display_of, message_id_of, role_of, Candidate, Menu, Model, Picker, DRAFT, FLOW,
};

/// 侧栏宽度与并排显示的最小面板宽。
const SIDEBAR_W: f32 = 228.0;
const SIDEBAR_INLINE_MIN_W: f32 = 640.0;
/// 输入盒内文字区高度：静止高度与内容超高后的上限（超出后在框内滚动），图片缩略图边长。
const INPUT_MIN_H: f32 = 96.0;
const INPUT_MAX_H: f32 = 240.0;
const THUMB: f32 = 160.0;
/// 浮层宽度、行高与边距。
const MENU_PLUS_W: f32 = 176.0;
const MENU_AGENT_W: f32 = 200.0;
const MENU_MODEL_W: f32 = 224.0;
const MENU_EFFORT_W: f32 = 224.0;
const MENU_ITEM_H: f32 = 28.0;
const MENU_GROUP_PAD_TOP: f32 = 6.0;
const MENU_GROUP_PAD_BOTTOM: f32 = 2.0;
const MENU_ITEM_PAD_X: f32 = 12.0;
const MENU_PAD_Y: f32 = 4.0;
/// 弹层与面板边缘的最小间距。
const POPUP_GAP: f32 = 6.0;
/// 工具排底边到面板底边的距离（输入区下留白 + 输入盒下内边距 + 工具排高）：菜单向上翻转时贴这条线。
const TOOL_ROW_BOTTOM: f32 = 10.0 + 8.0 + TOOL_ROW_H;
/// `#` 候选行高（单行 / 带次行的次行增量）、浮层最大高度与行内尺寸。
const PICKER_ROW_H: f32 = 25.0;
const PICKER_SUB_H: f32 = 15.0;
const PICKER_MAX_H: f32 = 224.0;
/// 候选框内「索引不完整」提示行的高度（单行 Micro + 图标）。
const PICKER_NOTICE_H: f32 = 18.0;
const PICKER_ROW_PAD_X: f32 = 10.0;
const PICKER_ROW_PAD_Y: f32 = 5.0;
const PICKER_LINE_GAP: f32 = 1.0;
const PICKER_ICON: f32 = 12.0;
const PICKER_ICON_GAP: f32 = 6.0;
/// 次行相对主行的缩进（= 图标列 + 图标间距）。
const PICKER_INDENT: f32 = PICKER_ICON + PICKER_ICON_GAP;
const PICKER_RADIUS: f32 = 6.0;
/// 输入盒内的工具排高度（发送钮 28px 定行高）。
const TOOL_ROW_H: f32 = 28.0;
/// 工具排的间隙、加号与发送钮的固定宽：两个触发钮只分剩下的宽。
const TOOL_GAP: f32 = 6.0;
const TOOL_PLUS_W: f32 = 24.0;
const TOOL_SEND_W: f32 = 28.0;
/// 触发钮收成图标时的宽与收起的门槛（低于它就装不下图标 + 文字）。
const TOOL_TRIGGER_ICON_W: f32 = 28.0;
const TOOL_TRIGGER_MIN_W: f32 = 72.0;
/// 输入盒内文字区的内边距（左 2 / 上 8 与拆分前的输入面一致）。
const AREA_PAD: Edges = Edges { left: 2.0, right: 2.0, top: 8.0, bottom: 0.0 };
/// 输入区的左右留白与输入盒内边距：算文字区可用宽时先扣掉它们。
const COMPOSER_PAD_X: f32 = 12.0;
const BOX_PAD: f32 = 8.0;
/// 附件托盘追加的高度（上留白 + 一行缩略名）。
const IMAGE_TRAY_H: f32 = 27.0;
/// 候选行高（单行 / 带次行）：默认字号档下「内衬 + 行高」的取整值，键盘巡航的滚动偏移据此算。
/// 略偏大是安全的——多滚一点只会让选中行更靠上，不会切掉它。
fn picker_row_height(candidate: &Candidate) -> f32 {
    if candidate.sub.is_empty() {
        PICKER_ROW_H
    } else {
        PICKER_ROW_H + PICKER_SUB_H
    }
}

/// 候选框实高：候选行 + 可选的提示行，超出上限即滚动。
fn picker_height(picker: &Picker) -> f32 {
    let rows = picker.candidates.iter().map(picker_row_height).sum::<f32>();
    let notice = if picker.partial { PICKER_NOTICE_H } else { 0.0 };
    (rows + notice).min(PICKER_MAX_H)
}

/// 文字区实际高度（含内边距）：随草稿行数在 `INPUT_MIN_H`–`INPUT_MAX_H` 之间增长。
fn area_height(ui: &mut Ui<'_, '_>, model: &Model, chat_width: f32) -> f32 {
    let width = (chat_width - COMPOSER_PAD_X * 2.0 - BOX_PAD * 2.0).max(1.0);
    ui.auto_area_height(
        DRAFT,
        &model.draft,
        TextRole::Body,
        width,
        INPUT_MIN_H,
        INPUT_MAX_H,
        AREA_PAD,
    )
}

/// 输入区实际占高：浮钮与菜单据此上移，不压住输入盒。
fn composer_height(ui: &mut Ui<'_, '_>, model: &Model, chat_width: f32) -> f32 {
    let box_h = BOX_PAD + area_height(ui, model, chat_width) + 4.0 + TOOL_ROW_H + BOX_PAD;
    1.0 + 8.0 + box_h + 10.0
        + if model.images.is_empty() { 0.0 } else { IMAGE_TRAY_H }
        + model.picker.as_ref().map(picker_height).unwrap_or(0.0)
}

/// 键盘巡航选中的候选：滚动偏移把该行底边带进可见窗口。
pub(crate) fn picker_scroll_offset(picker: &Picker, selected: usize) -> f32 {
    let viewport = picker_height(picker);
    let end = picker
        .candidates
        .iter()
        .take(selected + 1)
        .map(picker_row_height)
        .sum::<f32>();
    (end - viewport).max(0.0)
}

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
    if !model.follow_bottom && !model.messages.is_empty() {
        let width = (bounds.w - left).max(1.0);
        let y = (bounds.h - composer_height(ui, model, chat_width) - 40.0).max(0.0);
        let rect = Rect::new(left + (width - 96.0) / 2.0, y, 96.0, 28.0);
        // 被动浮层：浮钮不拦滚轮，上下翻消息流照常
        ui.overlay_passive(rect, |layer| {
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
    if model.menu.is_some() {
        // 弹层背板：整面承接外点（点外面关菜单），菜单本身随后声明压在上面
        ui.overlay(Rect::new(0.0, 0.0, bounds.w, bounds.h), |backdrop| {
            backdrop.hit_area("popup-backdrop");
        });
        popup_layer(ui, model, bounds, left);
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
            // 留白放进滚动内容：滚动条占最右缘一列，内容不压在其下
            stack.scroll("sessions", 2.0, |list| {
                list.pad(6.0, 6.0, |body| {
                    body.text_input("search", model.search.clone(), "搜索会话");
                    body.spacer(6.0);
                    for session in model.visible_sessions() {
                        session_row(body, model, session);
                    }
                    // 数据未到时不画空态，避免骨架帧把「暂无历史会话」挂在屏幕上
                    if model.ready && model.sessions.is_empty() {
                        let label = if model.search.trim().is_empty() {
                            "暂无历史会话"
                        } else {
                            "没有匹配的会话"
                        };
                        body.pad(8.0, 8.0, |empty| empty.caption(label.to_string()));
                    }
                });
            });
        });
    });
}

fn session_row(ui: &mut Ui<'_, '_>, model: &Model, session: &crate::model::SessionRow) {
    if let Some(rename) = model.rename.as_ref().filter(|edit| edit.id == session.id) {
        ui.pad(2.0, 2.0, |row| {
            row.text_input(format!("rename-input:{}", session.id), rename.title.clone(), "会话标题");
        });
        return;
    }
    let active = model.active.as_deref() == Some(session.id.as_str());
    let menu_open = model.session_menu.as_deref() == Some(session.id.as_str());
    let confirm = model.delete_confirm.as_deref() == Some(session.id.as_str());
    let row_id = format!("session:{}", session.id);
    let hint_id = row_id.clone();
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
                    let title = session.title().to_string();
                    text.elide_with_hint(
                        &hint_id,
                        title.clone(),
                        title,
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
                        format!("rename:{}", session.id),
                        "pen-line",
                        12.0,
                        20.0,
                        ColorToken::TextMuted,
                        true,
                    );
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
                        line.spinner(14.0, ColorToken::Accent);
                    } else {
                        line.hint("regen-title", "重新生成标题");
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
                line.hint("new", "新建会话");
                line.icon_button_named(
                    "new",
                    "file-plus",
                    13.0,
                    24.0,
                    ColorToken::TextSecondary,
                    true,
                );
                line.hint("toggle-sidebar", "历史会话");
                line.icon_button_named(
                    "toggle-sidebar",
                    "history",
                    13.0,
                    24.0,
                    ColorToken::TextSecondary,
                    true,
                );
                // 流式中不提供压缩入口：压缩要重写会话历史，与在途回复的写入互相踩
                if model.active.is_some() && model.messages.len() >= 2 && !model.active_streaming() {
                    if model.active_compacting() {
                        line.click_box(
                            "compact-progress",
                            BoxWidth::Fixed(24.0),
                            Edges::all(5.5),
                            0.0,
                            false,
                            false,
                            |busy| busy.spinner(13.0, ColorToken::TextMuted),
                        );
                    } else {
                        line.hint("compact", "压缩会话历史（折叠为摘要，原文保留）");
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
                    line.hint("clear-error", "清除提示");
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
        // 会话数据未到只留白：空态文案要等清单装载完成才成立
        if model.ready {
            empty_flow(ui);
        }
        return;
    }
    let viewport = (ui.bounds().h - 37.0 - composer_height(ui, model, chat_width)).max(160.0);
    // 滚动条贴面右缘：内容留白放进每条消息，右界落在轨道左侧
    ui.virtual_scroll(
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
        |item, index| {
            if index == 0 {
                item.spacer(12.0);
            }
            item.pad(12.0, 0.0, |inner| match marker {
                Some(marker) if index == marker => compaction_marker(inner, model),
                Some(marker) if index > marker => {
                    message_block(inner, model, &model.messages[index - 1], chat_width)
                }
                _ => message_block(inner, model, &model.messages[index], chat_width),
            });
            if index + 1 == len {
                item.spacer(12.0);
            }
        },
    );
}

fn empty_flow(ui: &mut Ui<'_, '_>) {
    ui.fill_column(0.0, |outer| {
        outer.fill_spacer();
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
            center.text_styled_fit(
                "开始新的 AI 对话",
                TextStyle::new(TextRole::Body).weight(TextWeight::SemiBold),
            );
        });
        outer.fill_spacer();
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
                            bubble.markdown_with(display.to_string(), mentions_of(message));
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
                            ImageSource::Vault { path: file.to_string(), max_edge: None },
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

/// 消息里的引用 → 解析器的引用候选：标签文本与草稿里插入的那段字面量逐字一致，胶囊才画得准。
fn mentions_of(message: &Value) -> Vec<MentionCandidate> {
    message
        .get("refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|reference| {
            let path = reference.get("path").and_then(Value::as_str)?;
            let label = reference.get("label").and_then(Value::as_str)?;
            let prefix = match reference.get("refKind").and_then(Value::as_str) {
                Some("table") => "表格",
                Some("canvas") => "画布",
                Some("dir") => "文件夹",
                _ => "笔记",
            };
            Some(MentionCandidate {
                key: path.to_string(),
                label: format!("{prefix}：{label}"),
            })
        })
        .collect()
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
        Edges::all(0.0),
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
                            // 摘要吃掉标题后的剩余宽度并按可用宽省略，不顶到卡片边缘硬截断
                            line.text_styled(one_line(&spec.summary, SUMMARY_CHARS), style);
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
            line.hint(&format!("copy:{id}"), "复制");
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
            line.hint(&format!("rollback:{id}"), "回到此处（该消息之后转为新话题）");
            line.icon_button_named(
                format!("rollback:{id}"),
                "corner-up-left",
                12.0,
                22.0,
                ColorToken::TextSecondary,
                true,
            );
            let actionable = model.is_last(id) && !model.active_streaming();
            line.hint(&format!("regen:{id}"), "重新生成回复");
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
                if let Some(picker) = &model.picker {
                    picker_box(input, picker, chat_width);
                }
                // 引用标签以浅强调底高亮；点进去整段选中、边界退格整段删除由输入面托管
                let font_size = input.role_size(TextRole::Body);
                input.text_area_submit_auto(
                    crate::model::DRAFT,
                    model.draft.clone(),
                    "输入消息，Enter 发送，Shift+Enter 换行",
                    INPUT_MIN_H,
                    INPUT_MAX_H,
                    font_size,
                    AREA_PAD,
                    model.marks(),
                );
                input.spacer(4.0);
                tool_row(input, model, chat_width);
            },
        );
        area.spacer(10.0);
    });
}

fn tool_row(ui: &mut Ui<'_, '_>, model: &Model, chat_width: f32) {
    let (slot, compact) = trigger_slot(chat_width);
    let triggers = show_triggers(chat_width);
    ui.row(TOOL_GAP, |line| {
        let plus_open = model.menu == Some(Menu::Plus);
        line.hint("plus-menu", "添加上下文与附件");
        line.icon_button_named(
            "plus-menu",
            "plus",
            13.0,
            TOOL_PLUS_W,
            if plus_open { ColorToken::Accent } else { ColorToken::TextSecondary },
            true,
        );
        if triggers {
            line.hint("agent-menu", &format!("Agent：{}", model.agent_label()));
            trigger(line, "agent-menu", "bot", model.agent_label(), slot, compact);
            line.hint(
                "model-menu",
                &format!("模型：{} · {}", model.model_label(), model.effort_label()),
            );
            trigger(
                line,
                "model-menu",
                "sparkles",
                format!("{} · {}", model.model_label(), model.effort_label()),
                slot,
                compact,
            );
        }
        line.grow();
        if model.active_streaming() {
            line.hint("stop", "停止生成");
            line.icon_button_filled(
                "stop",
                "square",
                14.0,
                TOOL_SEND_W,
                ColorToken::Surface,
                ColorToken::Text,
                true,
            );
        } else {
            line.hint("send", "发送（Enter）");
            line.icon_button_filled_color(
                "send",
                "arrow-up",
                14.0,
                TOOL_SEND_W,
                ColorToken::Accent,
                atelyx_plugin_protocol::paint::PaintColor::Rgba { rgba: [0x14, 0x14, 0x14, 255] },
                true,
            );
        }
    });
}

/// 工具排可用宽 = 输入区宽 − 输入区左右留白 − 输入盒内边距（与文字区、候选盒同口径）。
const TOOL_ROW_INSET: f32 = COMPOSER_PAD_X * 2.0 + BOX_PAD * 2.0;

/// 工具排可用宽：输入区宽扣掉输入区左右留白与输入盒内边距。
fn tool_row_width(chat_width: f32) -> f32 {
    (chat_width - TOOL_ROW_INSET).max(0.0)
}

/// 两个触发图标都装不下时只留加号与发送钮：结束回复的入口不能被挤掉。
fn show_triggers(chat_width: f32) -> bool {
    tool_row_width(chat_width)
        >= TOOL_PLUS_W + TOOL_TRIGGER_ICON_W * 2.0 + TOOL_SEND_W + TOOL_GAP * 4.0
}

/// 两个触发钮各得的宽与是否收成图标：先扣掉加号、发送钮与四个间隙。
fn trigger_slot(chat_width: f32) -> (f32, bool) {
    // 一行五子项（加号 / 两个触发钮 / 弹簧 / 发送钮）= 四个间隙
    let budget =
        (chat_width - TOOL_ROW_INSET - TOOL_PLUS_W - TOOL_SEND_W - TOOL_GAP * 4.0).max(0.0);
    let slot = (budget / 2.0).floor();
    (slot, slot < TOOL_TRIGGER_MIN_W)
}

fn trigger(ui: &mut Ui<'_, '_>, id: &str, icon: &str, label: String, slot: f32, compact: bool) {
    if compact {
        ui.icon_button_named(id, icon, 13.0, TOOL_TRIGGER_ICON_W, ColorToken::TextSecondary, true);
        return;
    }
    let width = (12.0 + 4.0 + ui.text_width(&label, TextRole::Micro) + 4.0 + 10.0 + 12.0).min(slot);
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

/// 菜单行高：行内文字与图标垂直居中在 28px 定高里；字号放大时按文字高撑开。
fn menu_item_height(ui: &mut Ui<'_, '_>) -> f32 {
    ui.text_height(TextRole::Caption).max(12.0).max(MENU_ITEM_H)
}

fn menu_group_height(ui: &mut Ui<'_, '_>) -> f32 {
    ui.text_height(TextRole::Micro) + MENU_GROUP_PAD_TOP + MENU_GROUP_PAD_BOTTOM
}

fn menu_width(menu: Menu) -> f32 {
    match menu {
        Menu::Plus => MENU_PLUS_W,
        Menu::Agent => MENU_AGENT_W,
        Menu::ModelRoot | Menu::ModelList => MENU_MODEL_W,
        Menu::Effort => MENU_EFFORT_W,
    }
}

/// 菜单内容高：与绘制同一口径逐行累加，弹层高度按它收缩（短菜单不出现滚动条）。
fn menu_body_height(ui: &mut Ui<'_, '_>, model: &Model, menu: Menu) -> f32 {
    let item = menu_item_height(ui);
    match menu {
        Menu::Plus | Menu::ModelRoot => item * 2.0,
        Menu::Agent => item * model.agents.len().max(1) as f32,
        Menu::Effort => item * 5.0,
        Menu::ModelList => {
            if model.models.is_empty() {
                return item;
            }
            let group = menu_group_height(ui);
            let mut total = 0.0;
            let mut provider = "";
            for row in &model.models {
                if provider != row.provider_name.as_str() {
                    provider = row.provider_name.as_str();
                    total += group;
                }
                total += item;
            }
            total
        }
    }
}

/// 菜单浮层：顶边贴触发钮的按下点，下方放不下且上方放得下时底边贴工具排上缘翻转；
/// 高度随内容收缩、超高才滚动。
fn popup_layer(ui: &mut Ui<'_, '_>, model: &Model, bounds: Rect, left: f32) {
    let Some(menu_kind) = model.menu else { return };
    let width = menu_width(menu_kind).min((bounds.w - POPUP_GAP * 2.0).max(1.0));
    let body = menu_body_height(ui, model, menu_kind);
    let card_h = (body + MENU_PAD_Y * 2.0).min((bounds.h - POPUP_GAP * 2.0).max(60.0));
    let (anchor_x, anchor_y) = model
        .menu_anchor
        .unwrap_or((left + MENU_ITEM_PAD_X, bounds.h - TOOL_ROW_BOTTOM));
    let x = anchor_x.clamp(POPUP_GAP, (bounds.w - width - POPUP_GAP).max(POPUP_GAP));
    let below = anchor_y + card_h;
    let trigger_top = bounds.h - TOOL_ROW_BOTTOM;
    let y = if below > bounds.h - POPUP_GAP && trigger_top - card_h >= POPUP_GAP {
        trigger_top - card_h
    } else {
        anchor_y
    };
    let y = y.clamp(POPUP_GAP, (bounds.h - card_h - POPUP_GAP).max(POPUP_GAP));
    let style = BoxStyle::new(Some(ColorToken::SurfaceOverlay), Some(ColorToken::Border), 10.0)
        .shadow(PaintShadow {
            offset_x: 0.0,
            offset_y: 6.0,
            blur: 18.0,
            color: PaintColor::Rgba { rgba: [0, 0, 0, 90] },
        });
    ui.overlay(Rect::new(x, y, width, card_h), |panel| {
        panel.boxed(
            BoxWidth::Fill,
            style,
            Edges { left: 0.0, right: 0.0, top: MENU_PAD_Y, bottom: MENU_PAD_Y },
            0.0,
            |menu| menu_box(menu, model, menu_kind, card_h - MENU_PAD_Y * 2.0),
        );
    });
}

/// `#` 候选：贴在输入盒内、输入面上方，占满可用宽度并按条数收高（超出滚动）。
/// 条目为双行卡片：主行文件名（按可用宽省略，不换行）+ 次行所在文件夹。
fn picker_box(ui: &mut Ui<'_, '_>, picker: &Picker, chat_width: f32) {
    let width = (chat_width - COMPOSER_PAD_X * 2.0 - BOX_PAD * 2.0 - 2.0).max(120.0);
    let name_w = (width - PICKER_ROW_PAD_X * 2.0 - PICKER_INDENT).max(1.0);
    let sub_w = (name_w - PICKER_INDENT).max(1.0);
    let rows: Vec<(&'static str, String, String)> = picker
        .candidates
        .iter()
        .map(|candidate| {
            let icon = if candidate.is_dir {
                "folder"
            } else {
                icon_of_path(&candidate.path)
            };
            let name = elide(ui, &candidate.name, TextRole::Caption, name_w);
            let sub = elide(ui, &candidate.sub, TextRole::Micro, sub_w);
            (icon, name, sub)
        })
        .collect();
    let selected = picker.selected;
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(Some(ColorToken::SurfaceMuted), Some(ColorToken::Border), 10.0).hairline(),
        Edges::all(0.0),
        0.0,
        |panel| {
            panel.scroll_max("picker", PICKER_MAX_H, 0.0, |list| {
                for (index, (icon, name, sub)) in rows.into_iter().enumerate() {
                    let current = index == selected;
                    list.click_box(
                        format!("pick:{index}"),
                        BoxWidth::Fill,
                        Edges::symmetric(PICKER_ROW_PAD_X, PICKER_ROW_PAD_Y),
                        PICKER_RADIUS,
                        current,
                        true,
                        |row| {
                            row.column(PICKER_LINE_GAP, |lines| {
                                lines.row(PICKER_ICON_GAP, |line| {
                                    let icon_color = if current {
                                        ColorToken::Accent
                                    } else {
                                        ColorToken::TextSecondary
                                    };
                                    line.icon_named(icon, PICKER_ICON, icon_color);
                                    let style =
                                        TextStyle::new(TextRole::Caption).color(if current {
                                            ColorToken::Text
                                        } else {
                                            ColorToken::TextSecondary
                                        });
                                    line.text_styled(name.clone(), style);
                                });
                                if !sub.is_empty() {
                                    lines.pad(PICKER_INDENT, 0.0, |sub_line| {
                                        sub_line.text_styled(
                                            sub.clone(),
                                            TextStyle::new(TextRole::Micro),
                                        );
                                    });
                                }
                            });
                        },
                    );
                }
                if picker.partial {
                    list.row(PICKER_ICON_GAP, |note| {
                        note.icon_named("triangle-alert", PICKER_ICON, ColorToken::Warning);
                        note.text_styled(
                            "文件索引不完整，仅列出已扫到的部分".to_string(),
                            TextStyle::new(TextRole::Micro).color(ColorToken::TextMuted),
                        );
                    });
                }
            });
        },
    );
}

/// 按可用宽度省略：放不下时取最长可容纳前缀 + `…`（与宿主同一口径，不换行）。
fn elide(ui: &mut Ui<'_, '_>, text: &str, role: TextRole, available: f32) -> String {
    if text.is_empty() || ui.text_width(text, role) <= available {
        return text.to_string();
    }
    let ellipsis = ui.text_width("…", role);
    if ellipsis > available {
        return String::new();
    }
    let mut end = 0usize;
    let mut width = 0.0;
    for (index, ch) in text.char_indices() {
        let advance = ui.text_width(&text[index..index + ch.len_utf8()], role);
        if width + advance + ellipsis > available {
            break;
        }
        width += advance;
        end = index + ch.len_utf8();
    }
    format!("{}…", &text[..end])
}

fn menu_box(ui: &mut Ui<'_, '_>, model: &Model, menu: Menu, height: f32) {
    ui.scroll_max("menu", height.max(1.0), 0.0, |list| match menu {
        Menu::Plus => {
            menu_row(list, "plus-context", "添加上下文", false, false);
            menu_row(list, "plus-attach", "上传附件", false, false);
        }
        Menu::Agent => {
            let current = model.active_agent();
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
                menu_note(list, "Agent 名册尚未读取");
            }
        }
        Menu::ModelRoot => {
            menu_row(
                list,
                "open-model",
                &format!("模型：{}", model.model_label()),
                false,
                true,
            );
            menu_row(
                list,
                "open-effort",
                &format!("推理等级：{}", model.effort_label()),
                false,
                true,
            );
        }
        Menu::ModelList => {
            let mut provider = "";
            for (index, row) in model.models.iter().enumerate() {
                if provider != row.provider_name.as_str() {
                    provider = row.provider_name.as_str();
                    menu_group(list, provider);
                }
                let selected = model
                    .model_pick
                    .as_ref()
                    .is_some_and(|(p, m)| p == &row.provider_id && m == &row.model);
                menu_row(list, &format!("model:{index}"), &row.label, selected, false);
            }
            if model.models.is_empty() {
                menu_note(list, "供应商未配置模型");
            }
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
                menu_row(list, &format!("effort:{wire}"), label, selected, false);
            }
        }
    });
}

/// 菜单行：定高 28（字号放大时按文字高撑开），选中项强调色、行尾对勾；子菜单箭头在行首。
fn menu_row(ui: &mut Ui<'_, '_>, id: &str, label: &str, checked: bool, submenu: bool) {
    let line = ui.text_height(TextRole::Caption).max(12.0);
    let pad_y = ((MENU_ITEM_H - line) / 2.0).max(0.0);
    ui.click_box(
        id,
        BoxWidth::Fill,
        Edges { left: MENU_ITEM_PAD_X, right: MENU_ITEM_PAD_X, top: pad_y, bottom: pad_y },
        0.0,
        false,
        true,
        |row| {
            row.row(6.0, |line| {
                if submenu {
                    line.icon_named("chevron-right", 12.0, ColorToken::TextMuted);
                }
                line.text_styled_fit(
                    label.to_string(),
                    TextStyle::new(TextRole::Caption).color(if checked {
                        ColorToken::Accent
                    } else {
                        ColorToken::Text
                    }),
                );
                if checked {
                    line.grow();
                    line.icon_named("check", 13.0, ColorToken::Accent);
                }
            });
        },
    );
}

/// 分组头：小字灰字，左 12、上 6 / 下 2（与菜单行同宽，不承接点击）。
fn menu_group(ui: &mut Ui<'_, '_>, label: &str) {
    ui.boxed(
        BoxWidth::Fill,
        BoxStyle::new(None, None, 0.0),
        Edges {
            left: MENU_ITEM_PAD_X,
            right: MENU_ITEM_PAD_X,
            top: MENU_GROUP_PAD_TOP,
            bottom: MENU_GROUP_PAD_BOTTOM,
        },
        0.0,
        |group| {
            group.text_styled_fit(label.to_string(), TextStyle::new(TextRole::Micro));
        },
    );
}

/// 空清单提示行：与菜单行同高，不承接点击。
fn menu_note(ui: &mut Ui<'_, '_>, label: &str) {
    let line = ui.text_height(TextRole::Caption).max(12.0);
    let pad_y = ((MENU_ITEM_H - line) / 2.0).max(0.0);
    ui.click_box(
        "menu-note",
        BoxWidth::Fill,
        Edges { left: MENU_ITEM_PAD_X, right: MENU_ITEM_PAD_X, top: pad_y, bottom: pad_y },
        0.0,
        false,
        false,
        |row| row.text_styled_fit(label.to_string(), TextStyle::new(TextRole::Caption)),
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

#[cfg(test)]
mod tests {
    use super::{
        picker_height, picker_scroll_offset, show_triggers, tool_row_width, trigger_slot,
        TOOL_GAP, TOOL_PLUS_W, TOOL_SEND_W, TOOL_TRIGGER_ICON_W, PICKER_NOTICE_H,
    };
    use crate::model::{Candidate, Picker};

    /// 工具排（含发送钮）必须落在输入盒内的可用宽里：超出去的子项既不画也点不到。
    #[test]
    fn tool_row_keeps_the_send_button_inside_the_row() {
        for width in [140.0f32, 180.0, 230.0, 320.0, 520.0] {
            let (slot, compact) = trigger_slot(width);
            let row = tool_row_width(width);
            let used = if show_triggers(width) {
                let triggers = if compact { TOOL_TRIGGER_ICON_W * 2.0 } else { slot * 2.0 };
                TOOL_PLUS_W + triggers + TOOL_SEND_W + TOOL_GAP * 4.0
            } else {
                TOOL_PLUS_W + TOOL_SEND_W + TOOL_GAP * 2.0
            };
            assert!(used <= row + 0.01, "{width} 宽下工具排超出行宽：{used} > {row}");
        }
        assert!(show_triggers(230.0), "230px 面板仍给两个触发钮留位");
        assert!(!show_triggers(140.0), "更窄时只留加号与发送钮");
        assert!(trigger_slot(230.0).1, "230px 面板下触发钮收成图标");
        assert!(!trigger_slot(520.0).1, "宽面板保留文字");
    }

    fn picker(rows: &[bool]) -> Picker {
        Picker {
            hash: 0,
            caret: 0,
            selected: 0,
            partial: false,
            candidates: rows
                .iter()
                .enumerate()
                .map(|(index, sub)| Candidate {
                    path: format!("文件{index}.md"),
                    is_dir: false,
                    name: format!("文件{index}.md"),
                    sub: if *sub { "资料".to_string() } else { String::new() },
                })
                .collect(),
        }
    }

    /// 「索引不完整」提示行也占候选框高度：输入区浮钮按它上移，少算会压住提示。
    #[test]
    fn picker_height_includes_the_partial_notice() {
        let mut picker = picker(&[false; 2]);
        let complete = picker_height(&picker);
        picker.partial = true;
        assert_eq!(picker_height(&picker), complete + PICKER_NOTICE_H);
    }

    #[test]
    fn picker_scroll_keeps_the_selected_row_in_view() {
        let compact = picker(&[false; 8]);
        assert_eq!(picker_scroll_offset(&compact, 0), 0.0);
        assert_eq!(picker_scroll_offset(&compact, 7), 0.0, "八行短条目不到限高，无需滚动");

        let tall = picker(&[true; 8]);
        assert_eq!(picker_scroll_offset(&tall, 4), 0.0, "首屏五行已容下");
        assert!(picker_scroll_offset(&tall, 7) > 0.0, "选中超出首屏的行要跟滚");
        assert_eq!(
            picker_scroll_offset(&tall, 7),
            8.0 * 40.0 - 224.0,
            "末行底边落在视口下缘"
        );
    }
}
