//! 对话消息流渲染段：面板消息区与画布对话卡共用的可嵌入纯渲染层。
//! 状态（markdown 解析缓存 / 图片句柄 / 折叠块展开集）经 [`StreamCtx`] 显式注入；
//! 消息操作钮由操作钩子提供，只读流（画布）不传钩子即无按钮。

use std::collections::{HashMap, HashSet};

use iced::widget::text::Span;
use iced::widget::{button, column, container, row};
use iced::{border, Length, Padding};
use serde_json::{json, Value};

use crate::chat_actions::ChatMsg;
use crate::markdown_select::{ChatSelection, SelectRun, SelectScope, SelectableRich};
use crate::markdown_view::LinkTarget;
use crate::ui::icon::icon;
use crate::ui::{controls, palette as pal};
use crate::ui::scroll;
use crate::ui::text;
use crate::Message;

/// 折叠态摘要的单行字符上限（中英文混排按字符计）。
const SUMMARY_LINE_CHARS: usize = 56;

/// 折叠块开合的消息路由（开合状态落在谁的视图状态里由调用方定）。
#[derive(Debug, Clone, Copy)]
pub enum ToggleRoute {
    /// 对话面板：`ChatState.expanded`。
    Panel,
    /// 画布对话卡：画布会话视图状态。
    Canvas,
}

/// 消息操作按钮行钩子：参数 = (消息, 是否 assistant)。只读流传 `None`。
pub type ActionsHook<'a> =
    Box<dyn for<'m> Fn(&'m Value, bool) -> iced::Element<'a, Message> + 'a>;

/// 消息流渲染上下文。
pub struct StreamCtx<'a> {
    /// 消息正文 markdown 解析缓存（键 = 源哈希 / 源 + 引用签名哈希；未命中回落纯文本）。
    pub docs: &'a HashMap<u64, atelyx_core::markdown::MarkdownDocument>,
    /// 图片句柄缓存（键 = 相对路径；未命中回落占位块）。
    pub images: &'a crate::markdown_view::ImageHandles,
    /// 折叠块展开集（键 = 消息 id + 步骤序）。
    pub expanded: &'a HashSet<String>,
    /// 主题（markdown 渲染取色）。
    pub theme: &'a iced::Theme,
    /// 渲染缩放系数（画布卡浮层传视口缩放；面板传 1.0）：字号、图标与留白均按此等比。
    pub scale: f32,
    /// 消息操作按钮行钩子（None = 只读流）。
    pub actions: Option<ActionsHook<'a>>,
    /// 流式中的消息 id（None = 无流式态；画布只读流恒 None）。
    pub streaming_id: Option<&'a str>,
    /// 折叠块开合的消息路由。
    pub toggle: ToggleRoute,
    /// 正文可拖选（对话面板；画布只读流为假）。
    pub selectable: bool,
    /// 应用层对话选区（按消息段隔离；`selectable` 为假时不消费）。
    pub selection: Option<&'a ChatSelection>,
}

/// 渲染消息列内容（无滚动容器，调用方自包 `scrollable`）：压缩标记 + 逐条消息。
pub fn message_column<'a>(
    messages: &'a [Value],
    session: &'a Value,
    ctx: StreamCtx<'a>,
) -> iced::Element<'a, Message> {
    // 压缩标记插在折叠块之后、首条未折叠消息之前（锚点由注解携带）
    let marker_idx = compaction_index(session);
    let s = ctx.scale;
    let mut col = column![].spacing(10.0 * s).padding(Padding::new(12.0 * s));
    for (i, m) in messages.iter().enumerate() {
        if Some(i) == marker_idx {
            if let Some(marker) = compaction_marker(session, s) {
                col = col.push(marker);
            }
        }
        let is_streaming_msg =
            ctx.streaming_id == Some(m.get("id").and_then(Value::as_str).unwrap_or(""));
        let message_id = m.get("id").and_then(Value::as_str).unwrap_or("");
        match m["role"].as_str() {
            Some("user") => col = col.push(user_bubble(m, part_scope(message_id, 0), &ctx)),
            Some("assistant") => {
                col = col.push(assistant_block(m, is_streaming_msg, message_id, &ctx))
            }
            _ => {}
        }
    }
    // 折叠覆盖到末尾（刚压缩完的常态）：标记落在列表最后
    if marker_idx == Some(messages.len()) {
        if let Some(marker) = compaction_marker(session, s) {
            col = col.push(marker);
        }
    }
    col.into()
}

/// 消息正文段标识：消息 id + 段序哈希（同一消息内的步骤与正文互不串选）。
fn part_scope(message_id: &str, part: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message_id.hash(&mut hasher);
    part.hash(&mut hasher);
    hasher.finish()
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

/// 压缩注解标记（锚点为最后一条消息时渲染于列表末尾）。
fn compaction_marker(session: &Value, scale: f32) -> Option<iced::Element<'static, Message>> {
    let c = session.get("compaction")?;
    if c.is_null() {
        return None;
    }
    let summary = c.get("summary").and_then(Value::as_str).unwrap_or("");
    let count = c.get("messageCount").and_then(Value::as_u64).unwrap_or(0);
    let mut col = column![].spacing(4.0 * scale);
    col = col.push(
        text(format!("上下文已压缩 · 以上 {count} 条已折叠为摘要"))
            .size(pal::fs_micro() * scale)
            .color(pal::TEXT_MUTED),
    );
    if !summary.is_empty() {
        col = col.push(
            text(summary.to_string())
                .size(pal::fs_micro() * scale)
                .color(pal::TEXT_SECONDARY),
        );
    }
    Some(
        container(col)
            .width(Length::Fill)
            .padding(8.0 * scale)
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
fn user_bubble<'a>(m: &'a Value, scope: u64, ctx: &StreamCtx<'a>) -> iced::Element<'a, Message> {
    // displayContent 存在即以它为正文（可为空——纯图消息）；缺失（旧消息）回落 content
    let source = match m.get("displayContent") {
        Some(Value::String(s)) => s.as_str(),
        _ => m.get("content").and_then(Value::as_str).unwrap_or(""),
    };
    // 引用标签签名与解析缓存同源（前缀：标签），命中才走 mention 胶囊文档
    let sig = crate::chat_actions::mention_sig(m);
    let s = ctx.scale;
    let mut col = column![].spacing(4.0 * s);
    // 正文：单行纯文本直接按文本渲染（Markdown 整段渲染恒撑满宽度，短句会得到
    // 一个巨大的空盒子）；带引用的消息用 mention 候选解析（胶囊高亮）
    let body: iced::Element<'a, Message> = if !sig.is_empty() {
        doc_by_key(ctx, source, crate::chat_actions::doc_key(source, &sig), scope)
    } else if is_plain_line(source) {
        plain_body(ctx, source, scope)
    } else {
        markdown_doc(ctx, source, scope)
    };
    col = col.push(
        container(body)
            .max_width(560.0 * s)
            .padding(10.0 * s)
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
        .map(|rel| attachment_thumb(ctx, rel));
    let thumbs: Vec<_> = thumbs.collect();
    if !thumbs.is_empty() {
        col = col.push(row(thumbs).spacing(6.0 * s));
    }
    if let Some(actions) = &ctx.actions {
        col = col.push(actions(m, false));
    }
    container(container(col).align_x(iced::alignment::Horizontal::Right).width(Length::Fill))
        .width(Length::Fill)
        .into()
}

/// 附件缩略图（上限 160px 见方；未载入显示占位块）。
fn attachment_thumb<'a>(ctx: &StreamCtx<'a>, rel: &str) -> iced::Element<'a, Message> {
    let s = ctx.scale;
    let placeholder = || {
        container(icon("image", 20.0 * s, pal::TEXT_MUTED))
            .width(96.0 * s)
            .height(64.0 * s)
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
    match ctx.images.get(rel) {
        Some(handle) => iced::widget::image(handle.clone())
            .width(Length::Fixed(160.0 * s))
            .height(Length::Fixed(160.0 * s))
            .filter_method(iced::widget::image::FilterMethod::Linear)
            .into(),
        None => placeholder(),
    }
}

/// assistant 块（无气泡全宽）：步骤块（思考 / 工具可折叠 / 叙述行）+ 最终正文 + 操作。
fn assistant_block<'a>(
    m: &'a Value,
    streaming: bool,
    message_id: &str,
    ctx: &StreamCtx<'a>,
) -> iced::Element<'a, Message> {
    let content = m.get("content").and_then(Value::as_str).unwrap_or("");
    let steps = m.get("steps").and_then(Value::as_array);
    let s = ctx.scale;
    let final_scope = part_scope(message_id, steps.map_or(0, |s| s.len()) + 1);
    let mut col = column![].spacing(6.0 * s);
    if let Some(steps) = steps {
        let last_step = steps.len().saturating_sub(1);
        for (idx, s) in steps.iter().enumerate() {
            let key = format!("{message_id}:{idx}");
            match s.get("kind").and_then(Value::as_str) {
                Some("reasoning") => {
                    col = col.push(reasoning_block(ctx, s, streaming && idx == last_step, &key))
                }
                Some("text") => {
                    let t = s.get("text").and_then(Value::as_str).unwrap_or("");
                    if !t.is_empty() {
                        col = col.push(markdown_doc(ctx, t, part_scope(message_id, idx + 1)));
                    }
                }
                Some("tool") => col = col.push(tool_block(ctx, s, &key)),
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
        col = col.push(markdown_doc(ctx, content, final_scope));
    } else if streaming && steps.map(|ss| ss.is_empty()).unwrap_or(true) {
        col = col.push(text("生成中…").size(pal::fs_caption() * s).color(pal::TEXT_MUTED));
    }
    if let Some(actions) = &ctx.actions {
        col = col.push(actions(m, true));
    }
    container(col).width(Length::Fill).into()
}

/// 思考块：折叠态一行摘要（流式取最新一行、完成后取首行）。
fn reasoning_block<'a>(
    ctx: &StreamCtx<'a>,
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
        ctx,
        key,
        StepBlock {
            leading: vec![icon("brain", 12.0 * ctx.scale, pal::TEXT_MUTED)],
            title: "思考过程".to_string(),
            summary: summary.to_string(),
            mono_summary: false,
            body: full.to_string(),
            empty_hint: "思考内容为空",
        },
    )
}

/// 工具块：折叠态 = 状态色图示 + 工具名 + 等宽参数/结果摘要；展开态 = 参数与结果分节。
fn tool_block<'a>(ctx: &StreamCtx<'a>, s: &'a Value, key: &str) -> iced::Element<'a, Message> {
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
        ctx,
        key,
        StepBlock {
            leading: vec![
                icon(tool_icon(&name), 12.0 * ctx.scale, pal::TEXT_MUTED),
                icon(status_icon, 11.0 * ctx.scale, status_color),
            ],
            title: crate::settings_view::ai::tool_label(&name),
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
/// 展开集由调用方自持（键 = 消息 id + 步骤序），开合经 [`ToggleRoute`] 路由。
/// 折叠块摘要单行化：换行折叠为空格 + 按字符数截断加省略号（工具结果天然多行）。
fn one_line(s: &str, cap_chars: usize) -> String {
    let mut out: String = s.chars().filter(|c| !c.is_control()).take(cap_chars + 1).collect();
    if out.chars().count() > cap_chars {
        out = out.chars().take(cap_chars).collect();
        out.push('\u{2026}');
    }
    out
}

fn details_block<'a>(
    ctx: &StreamCtx<'a>,
    key: &str,
    spec: StepBlock<'a>,
) -> iced::Element<'a, Message> {
    let StepBlock {
        leading,
        title,
        summary,
        mono_summary,
        body,
        empty_hint,
    } = spec;
    let open = ctx.expanded.contains(key);
    let s = ctx.scale;
    let mut head = row![icon(
        if open { "chevron-down" } else { "chevron-right" },
        12.0 * s,
        pal::TEXT_MUTED
    )]
    .spacing(6.0 * s)
    .align_y(iced::alignment::Vertical::Center);
    for item in leading {
        head = head.push(item);
    }
    head = head.push(text(title).size(pal::fs_micro() * s).color(pal::TEXT_SECONDARY));
    if !summary.is_empty() {
        // 折叠态头部恒一行：超长摘要视图层截断（展开正文仍完整），防头部被撑成多行
        let mut summary_text =
            text(one_line(&summary, SUMMARY_LINE_CHARS))
                .size(pal::fs_micro() * s)
                .color(pal::TEXT_MUTED);
        if mono_summary {
            summary_text = summary_text.font(crate::ui::mono());
        }
        head = head.push(summary_text);
    }
    let toggle_msg = match ctx.toggle {
        ToggleRoute::Panel => Message::Chat(ChatMsg::ToggleStep(key.to_string())),
        ToggleRoute::Canvas => {
            Message::Canvas(crate::canvas_view::CanvasMsg::StreamToggle(key.to_string()))
        }
    };
    let header = button(container(head).width(Length::Fill))
        .width(Length::Fill)
        .padding(Padding::ZERO.vertical(5.0 * s))
        .style(controls::row_style(false, pal::RADIUS_XS))
        .on_press(toggle_msg);
    let mut col = column![header];
    if open {
        if !body.is_empty() {
            col = col.push(controls::h_divider_subtle());
            col = col.push(
                scroll::v_scaled(
                    container(
                        text(body.to_string())
                            .size(pal::fs_micro() * s)
                            .color(pal::TEXT_SECONDARY),
                    )
                    .width(Length::Fill)
                    .padding(Padding::ZERO.vertical(6.0 * s)),
                    s,
                )
                .height(160.0 * s),
            );
        } else if !empty_hint.is_empty() {
            col = col.push(
                container(
                    text(empty_hint.to_string())
                        .size(pal::fs_micro() * s)
                        .color(pal::TEXT_MUTED),
                )
                .padding(Padding::ZERO.vertical(6.0 * s)),
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

/// 单行纯文本判定（无换行、无 Markdown 结构记号、短行）= 可直接按文本渲染并按内容收缩。
fn is_plain_line(source: &str) -> bool {
    !source.contains('\n')
        && source.chars().count() <= 200
        && !source.contains(['*', '_', '`', '[', ']', '#', '>', '|', '\\', '~'])
}

/// 单行纯文本行高：对齐 `text()` 的默认行高，短句气泡的行盒高度不随可拖选档改变。
const PLAIN_LINE_HEIGHT: f32 = 1.3;

/// 单行纯文本消息体（气泡按内容收缩）：可拖选档用单片段自绘，其余走轻量文本。
fn plain_body<'a>(ctx: &StreamCtx<'a>, source: &'a str, scope: u64) -> iced::Element<'a, Message> {
    if !ctx.selectable {
        return text(source.to_string())
            .size(pal::fs_body() * ctx.scale)
            .color(pal::TEXT_PRIMARY)
            .into();
    }
    let scope =
        SelectScope { id: scope, doc_key: crate::chat_actions::source_hash(source) };
    SelectableRich::new(
        vec![Span::<LinkTarget, iced::Font>::new(source).font(crate::ui::regular())],
        vec![SelectRun::new((0, source.len()), source.len())],
    )
    .size(pal::fs_body() * ctx.scale)
    .line_height(PLAIN_LINE_HEIGHT)
    .color_maybe(Some(pal::TEXT_PRIMARY))
    .width(Length::Shrink)
    .chat_selection(scope, ctx.selection)
    .into()
}

/// 消息 markdown 渲染（复用阅读面管线；解析缓存由调用方维护，未命中回落纯文本）。
fn markdown_doc<'a>(
    ctx: &StreamCtx<'a>,
    source: &'a str,
    scope: u64,
) -> iced::Element<'a, Message> {
    doc_by_key(ctx, source, crate::chat_actions::source_hash(source), scope)
}

/// 按缓存键取解析文档渲染（键由调用方决定：普通 = 源哈希；带引用 = 源 + 引用签名）。
fn doc_by_key<'a>(
    ctx: &StreamCtx<'a>,
    source: &'a str,
    key: u64,
    scope: u64,
) -> iced::Element<'a, Message> {
    static EMPTY_IMAGES: std::sync::OnceLock<crate::markdown_view::ImageHandles> =
        std::sync::OnceLock::new();
    let images = EMPTY_IMAGES.get_or_init(std::collections::HashMap::new);
    match ctx.docs.get(&key) {
        Some(doc) if ctx.selectable => crate::markdown_view::document_body_select(
            doc,
            ctx.theme,
            images,
            true,
            ctx.scale,
            SelectScope { id: scope, doc_key: key },
            ctx.selection,
        ),
        Some(doc) => {
            crate::markdown_view::document_body_scaled(doc, ctx.theme, images, true, ctx.scale)
        }
        None => text(source.to_string())
            .size(pal::fs_body() * ctx.scale)
            .color(pal::TEXT_PRIMARY)
            .into(),
    }
}
