//! 对话卡：发送链路（引用注入 / 图片落盘 / 直写 run）、run 事件收敛、重生成 / 回滚 / 分支、
//! 压缩与产出落卡。消息真源在卡节点 `data.messages`，落盘由保存链统一负责。

use atelyx_format::canvas::geometry::{display_size, find_free_spot};
use atelyx_format::canvas::{CanvasFileEdge, CanvasFileNode};
use atelyx_plugin_protocol::method::{
    self, ChatDirectCompactParams, ChatDirectParams, ChatEvent, ChatEventParams,
};
use serde_json::{Value, json};

use crate::model::{CardChat, SendPlan, Session};
use crate::refs::{self, CardContent, CardSource, RefEntry, RefKind};
use crate::{PluginState, now_secs, uuid};

/// 画布卡会话键前缀（AI 事件按它归属到卡）。
pub const SESSION_PREFIX: &str = "canvas:";
/// 落卡与源卡的间距（世界坐标）。
pub const CARD_GAP: f64 = 40.0;
/// 文本卡默认尺寸。
pub const TEXT_CARD_W: f64 = 320.0;
pub const TEXT_CARD_H: f64 = 240.0;
/// 落卡避让步长。
pub const SPOT_STEP: f64 = 24.0;

/// 卡会话键。
pub fn session_id(node: &str) -> String {
    format!("{SESSION_PREFIX}{node}")
}

/// 从会话键取回卡节点 id。
pub fn node_of_session(session: &str) -> Option<&str> {
    session.strip_prefix(SESSION_PREFIX)
}

/// 附件临时区的会话 id：非字母数字 / `-` 的字符一律替换为 `-`，截断 128（目录名不接受路径语义）。
pub fn media_session_id(node: &str) -> String {
    let body: String = node
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .take(128)
        .collect();
    format!("canvas-{body}")
}

/// 图片文件名的扩展名（小写，受支持集合内）。
pub fn image_ext_of(name: &str) -> Option<String> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif").then_some(ext)
}

/// 扩展名 → MIME（附件落盘后写进消息）。
pub fn image_mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// 消息上的操作入口（生成与解析同一套编码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageAction {
    Copy,
    Rollback,
    RollbackConfirm,
    CancelRollback,
    Branch,
    Regen,
}

impl MessageAction {
    pub fn key(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Rollback => "rollback",
            Self::RollbackConfirm => "rollback-confirm",
            Self::CancelRollback => "cancel-rollback",
            Self::Branch => "branch",
            Self::Regen => "regen",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "copy" => Self::Copy,
            "rollback" => Self::Rollback,
            "rollback-confirm" => Self::RollbackConfirm,
            "cancel-rollback" => Self::CancelRollback,
            "branch" => Self::Branch,
            "regen" => Self::Regen,
            _ => return None,
        })
    }
}

/// 消息操作 id：`canvas-msg-<动作>:<节点 id>[:<消息 id>]`。
/// 节点 / 消息 id 自身含任意字符都不影响解析（按 `:` 分段，不做前缀匹配）。
pub fn message_action_id(action: MessageAction, node: &str, message: Option<&str>) -> String {
    match message {
        Some(message) => format!("canvas-msg-{}:{node}:{message}", action.key()),
        None => format!("canvas-msg-{}:{node}", action.key()),
    }
}

/// 解析消息操作 id；非本形态或段为空返回 None。
pub fn parse_message_action(id: &str) -> Option<(MessageAction, &str, Option<&str>)> {
    let rest = id.strip_prefix("canvas-msg-")?;
    let mut parts = rest.splitn(3, ':');
    let action = MessageAction::from_key(parts.next()?)?;
    let node = parts.next().filter(|node| !node.is_empty())?;
    let message = match parts.next() {
        Some(message) if !message.is_empty() => Some(message),
        Some(_) => return None,
        None => None,
    };
    Some((action, node, message))
}

/// 卡头展示用的引用标题（文本卡取文件名，其余取 data 上的标题/文件名）。
pub fn card_ref_title(node: &CanvasFileNode) -> String {
    let file = node.data.get("file").and_then(Value::as_str);
    match node.node_type.as_str() {
        "text" => file
            .map(crate::model::file_title)
            .or_else(|| {
                node.data
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "文本".to_string()),
        _ => node
            .data
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                node.data
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| file.map(crate::model::file_title))
            .unwrap_or_else(|| "卡片".to_string()),
    }
}

/// `#` 候选的一行展示文本（前缀 + 名称）。
pub fn candidate_label(target: &crate::model::MentionTarget) -> String {
    match target {
        crate::model::MentionTarget::Vault { rel, is_dir } => {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            if *is_dir {
                format!("#文件夹：{name}/")
            } else {
                format!(
                    "#{}：{name}",
                    crate::refs::kind_of_file(rel)
                        .map(|kind| kind.prefix())
                        .unwrap_or("笔记")
                )
            }
        }
        crate::model::MentionTarget::Card { node_id } => format!("#卡片：{node_id}"),
    }
}

/// 对话卡的消息列表。
pub fn messages_of<'a>(session: &'a Session, node: &str) -> Option<&'a [Value]> {
    session
        .node(node)?
        .data
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
}

/// 写入 / 替换一条消息（按 id 定位）；相同内容不写。
pub fn upsert_message(session: &mut Session, node_id: &str, message: Value) -> bool {
    let Some(node) = session.node_mut(node_id) else {
        return false;
    };
    let Some(object) = node.data.as_object_mut() else {
        return false;
    };
    let messages = object
        .entry("messages")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(array) = messages.as_array_mut() else {
        return false;
    };
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(id) = id {
        if let Some(slot) = array
            .iter_mut()
            .find(|m| m.get("id").and_then(Value::as_str) == Some(&id))
        {
            if *slot == message {
                return false;
            }
            *slot = message;
            return true;
        }
    }
    array.push(message);
    true
}

/// 删除一条消息；返回是否删除。
pub fn remove_message(session: &mut Session, node_id: &str, message_id: &str) -> bool {
    let Some(node) = session.node_mut(node_id) else {
        return false;
    };
    let Some(array) = node.data.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let before = array.len();
    array.retain(|m| m.get("id").and_then(Value::as_str) != Some(message_id));
    array.len() != before
}

/// 整体替换消息列表。
pub fn replace_messages(session: &mut Session, node_id: &str, messages: Vec<Value>) {
    if let Some(node) = session.node_mut(node_id) {
        node.data["messages"] = Value::Array(messages);
    }
}

/// 给对话卡补默认运行态 / 清掉已删卡的运行态。
pub fn sync_card_chats(session: &mut Session) {
    let alive: Vec<String> = session
        .data
        .nodes
        .iter()
        .filter(|n| n.node_type == "conversation")
        .map(|n| n.id.clone())
        .collect();
    session.view.card_chat.retain(|id, _| alive.contains(id));
    for id in alive {
        session.view.card_chat.entry(id).or_default();
    }
}

/// 卡片配置：节点 data 上的显式选择优先，缺省回落仓库当前选择。
pub fn card_pick(session: &Session, node: &str) -> (Option<String>, Option<(String, String)>) {
    let Some(card) = session.node(node) else {
        return (None, None);
    };
    let agent = card
        .data
        .get("agentId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let provider = card
        .data
        .get("providerId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let model = card
        .data
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let pick = match (provider, model) {
        (Some(p), Some(m)) => Some((p.to_string(), m.to_string())),
        _ => None,
    };
    (agent, pick)
}

/// 卡片推理等级。
pub fn card_effort(session: &Session, node: &str) -> Option<String> {
    session
        .node(node)?
        .data
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 该卡是否被对端持有编辑锁。
pub fn locked_by_peer(session: &Session, node: &str) -> bool {
    session.view.locked_by_peer(&session.file, node).is_some()
}

/// 卡当前是否在流式生成。
pub fn is_streaming(session: &Session, node: &str) -> bool {
    session
        .view
        .card_chat
        .get(node)
        .is_some_and(|chat| chat.streaming)
}

/// 任一卡正在流式生成（撤销 / 重做 / 回滚一律让位）。
pub fn any_streaming(session: &Session) -> bool {
    session.any_streaming()
}

// ===== 发送 =====

/// 取草稿并发起发送：校验 → 图片落盘 → 引用注入 → 直写 run。
pub async fn send(plugin: &PluginState, ctx: &atelyx_plugin_sdk::Context, node_id: &str) -> bool {
    let Some(plan) = plugin
        .with_session_mut(|session| {
            let (agent_id, pick) = card_pick(session, node_id);
            let effort = card_effort(session, node_id);
            if is_streaming(session, node_id) || locked_by_peer(session, node_id) {
                return None;
            }
            let chat = session.view.card_chat.get_mut(node_id)?;
            let display = chat.draft.trim().to_string();
            // 纯图片消息合法：无正文时只要有待发图片就放行
            if display.is_empty() && chat.pending_images.is_empty() {
                return None;
            }
            let plan = SendPlan {
                node: node_id.to_string(),
                display,
                draft: chat.draft.clone(),
                refs: std::mem::take(&mut chat.mention_refs),
                images: std::mem::take(&mut chat.pending_images),
                agent_id,
                model_pick: pick,
                effort,
            };
            chat.mention_picker = None;
            chat.error = None;
            Some(plan)
        })
        .flatten()
    else {
        return false;
    };

    let sid = media_session_id(node_id);
    let mut attachments: Vec<Value> = Vec::new();
    let mut written: Vec<String> = Vec::new();
    for image in &plan.images {
        let Some(ext) = image_ext_of(&image.file_name) else {
            cleanup_written(ctx, &written).await;
            restore(plugin, &plan);
            let _ = ctx
                .notify("只能发送图片附件", None, method::NotificationLevel::Warning)
                .await;
            return false;
        };
        let rel = format!(".atelyx/temp/chats/{sid}/img-{}.{ext}", nanoid::nanoid!());
        if let Err(error) = ctx.vault_write_binary(&rel, &image.bytes).await {
            cleanup_written(ctx, &written).await;
            restore(plugin, &plan);
            let _ = ctx
                .notify(
                    "图片落盘失败，消息未发送",
                    Some(&format!("{rel}：{error}")),
                    method::NotificationLevel::Error,
                )
                .await;
            return false;
        }
        written.push(rel.clone());
        attachments.push(json!({ "kind": "image", "mime": image.mime, "file": rel }));
    }

    let (hits, path_block, card_block, image_files, config) = plugin
        .with_session(|session| {
            let hits = refs::scan_hits(&plan.display, &plan.refs);
            let path_block = refs::reference_path_block(&hits);
            let (card_block, image_files) = card_injection(session, &hits, node_id);
            let (agent_id, pick) = card_pick(session, node_id);
            let config = (
                plan.agent_id.clone().or(agent_id),
                plan.model_pick.clone().or(pick),
                plan.effort
                    .clone()
                    .or_else(|| card_effort(session, node_id)),
            );
            (hits, path_block, card_block, image_files, config)
        })
        .unwrap_or_default();

    for file in image_files {
        if attachments
            .iter()
            .any(|a| a.get("file").and_then(Value::as_str) == Some(file.as_str()))
        {
            continue;
        }
        let ext = file
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        attachments.push(json!({ "kind": "image", "mime": image_mime(&ext), "file": file }));
    }

    let mut content =
        String::with_capacity(path_block.len() + card_block.len() + plan.display.len());
    content.push_str(&path_block);
    content.push_str(&card_block);
    content.push_str(&plan.display);
    let refs_json: Vec<Value> = plan
        .refs
        .iter()
        .filter(|entry| {
            hits.iter().any(|hit| match (&hit.node_id, &entry.node_id) {
                (Some(a), Some(b)) => a == b,
                _ => hit.path == entry.path,
            })
        })
        .map(|entry| {
            let mut value = json!({
                "kind": "file",
                "label": entry.label,
                "refKind": entry.kind.key(),
            });
            if entry.kind != RefKind::Card || entry.node_id.as_deref() != Some(entry.path.as_str())
            {
                value["path"] = json!(entry.path);
            }
            if let Some(id) = &entry.node_id {
                value["nodeId"] = json!(id);
            }
            value
        })
        .collect();

    let mut added_edges: Vec<String> = Vec::new();
    for id in plan.refs.iter().filter_map(|entry| entry.node_id.clone()) {
        let edge = plugin.with_session_mut(|session| ensure_edge(session, &id, node_id)).flatten();
        added_edges.extend(edge);
    }

    let user_message = json!({
        "id": uuid(),
        "role": "user",
        "content": content,
        "displayContent": plan.display,
        "refs": refs_json,
        "attachments": attachments,
        "createdAt": now_ms(),
    });
    let history = plugin
        .with_session_mut(|session| {
            if !upsert_message(session, node_id, user_message.clone()) {
                return None;
            }
            let history = messages_of(session, node_id)
                .unwrap_or(&[])
                .iter()
                .filter(|m| m.get("id") != user_message.get("id"))
                .cloned()
                .collect::<Vec<_>>();
            session.touch();
            if let Some(chat) = session.view.card_chat.get_mut(node_id) {
                // 用户消息已落卡，草稿到此才清空（失败路径才能原样还回）
                chat.draft.clear();
                chat.caret = 0;
                chat.mention_refs.clear();
                chat.pending_images.clear();
                chat.streaming = true;
                chat.follow_bottom = true;
                chat.error = None;
                chat.rollback_confirm = None;
            }
            Some(history)
        })
        .flatten();
    let Some(history) = history else {
        cleanup_written(ctx, &written).await;
        restore(plugin, &plan);
        return false;
    };

    if let Err(error) = run(plugin, ctx, node_id, history, user_message.clone(), config).await {
        // 发送没成立：撤掉刚插入的用户消息，草稿与引用还给输入区，清掉本次落盘的图片
        let message_id = user_message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        plugin.with_session_mut(|session| {
            remove_message(session, node_id, &message_id);
            // 本次引用新建的连线也一并撤掉：发送没成立，卡之间不该多出一条引用边
            session.data.edges.retain(|edge| !added_edges.contains(&edge.id));
            session.touch();
        });
        restore(plugin, &plan);
        cleanup_written(ctx, &written).await;
        fail(plugin, node_id, error);
        return false;
    }
    let paths: Vec<String> = attachments
        .iter()
        .filter(|a| a.get("kind").and_then(Value::as_str) == Some("image"))
        .filter_map(|a| a.get("file").and_then(Value::as_str).map(str::to_string))
        .collect();
    crate::session::load_refs_for(plugin, ctx, paths).await;
    true
}

/// 发送失败：正文（原样，不去空白）、引用与全部待发图片还给输入区。
fn restore(plugin: &PluginState, plan: &SendPlan) {
    plugin.with_session_mut(|session| {
        let Some(chat) = session.view.card_chat.get_mut(&plan.node) else {
            return;
        };
        chat.draft = plan.draft.clone();
        chat.caret = plan.draft.len();
        chat.mention_refs = plan.refs.clone();
        chat.pending_images = plan.images.clone();
        chat.streaming = false;
    });
}

/// 起一轮直写 run：消息真源在卡上，宿主解析配置后交给 AI 核心。
async fn run(
    plugin: &PluginState,
    ctx: &atelyx_plugin_sdk::Context,
    node_id: &str,
    history: Vec<Value>,
    user_message: Value,
    config: (Option<String>, Option<(String, String)>, Option<String>),
) -> Result<(), String> {
    let (agent_id, pick, effort) = config;
    let compaction = plugin
        .with_session(|session| {
            session
                .node(node_id)
                .and_then(|n| n.data.get("compaction"))
                .cloned()
        })
        .flatten()
        .filter(|value| !value.is_null());
    let (model, provider_id) = match pick {
        Some((provider, model)) => (Some(model), Some(provider)),
        None => (None, None),
    };
    let params = ChatDirectParams {
        session_id: session_id(node_id),
        history,
        user_message,
        compaction,
        agent_id,
        model,
        provider_id,
        effort,
        space_prompt: None,
    };
    let value = serde_json::to_value(params).map_err(|error| error.to_string())?;
    ctx.call_raw(method::ai::CHAT_SEND_DIRECT, value)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// 清掉本次发送已落盘的临时图片；失败只记日志，不覆盖发送结果。
async fn cleanup_written(ctx: &atelyx_plugin_sdk::Context, paths: &[String]) {
    for path in paths {
        if let Err(error) = ctx.vault_remove(path).await {
            ctx.log_error(&format!("清理未发送的图片失败（{path}）：{error}"));
        }
    }
}

fn fail(plugin: &PluginState, node_id: &str, error: String) {
    plugin.with_session_mut(|session| {
        if let Some(chat) = session.view.card_chat.get_mut(node_id) {
            chat.streaming = false;
            chat.error = Some(error);
        }
    });
}

/// 引用命中 → 卡类型注入内容（图片附件路径单独返回）。
fn card_injection(
    session: &Session,
    hits: &[refs::RefHit],
    conversation: &str,
) -> (String, Vec<String>) {
    let mut contents: Vec<CardContent> = Vec::new();
    for hit in hits {
        if hit.kind != RefKind::Card {
            continue;
        }
        let Some(source_id) = hit.node_id.as_deref() else {
            continue;
        };
        if source_id == conversation {
            continue;
        }
        let Some(source) = session.node(source_id) else {
            continue;
        };
        let title = card_ref_title(source);
        let content = match source.node_type.as_str() {
            "text" => match source.data.get("file").and_then(Value::as_str) {
                Some(file) => match session.view.refs.get(file) {
                    Some(crate::model::RefContent::Text(text)) => CardSource::MediaText {
                        title,
                        body: atelyx_format::frontmatter::parse(text).body,
                    },
                    _ => CardSource::Missing { title },
                },
                None => CardSource::InlineText {
                    title,
                    body: source
                        .data
                        .get("bodyMd")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                },
            },
            "table" => match source
                .data
                .get("file")
                .and_then(Value::as_str)
                .and_then(|file| session.view.refs.get(file))
            {
                Some(crate::model::RefContent::Table { body, .. }) => CardSource::TableText {
                    title,
                    body: body.clone(),
                },
                _ => CardSource::Missing { title },
            },
            "media" => {
                let kind = source
                    .data
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("file");
                let file = source.data.get("file").and_then(Value::as_str);
                match (kind, file) {
                    ("image", Some(file)) => match session.view.refs.get(file) {
                        Some(crate::model::RefContent::Missing) => CardSource::Missing { title },
                        _ => CardSource::Image {
                            title,
                            file: file.to_string(),
                        },
                    },
                    (_, Some(file)) => match session.view.refs.get(file) {
                        Some(crate::model::RefContent::Text(body)) => CardSource::MediaText {
                            title,
                            body: body.clone(),
                        },
                        Some(crate::model::RefContent::Binary) => CardSource::Binary { title },
                        _ => CardSource::Missing { title },
                    },
                    _ => CardSource::Missing { title },
                }
            }
            _ => continue,
        };
        contents.push(CardContent {
            node_id: source_id.to_string(),
            source: content,
        });
    }
    refs::card_injection(hits, &contents)
}

/// 确保一条对话卡连线（同向且非无向边去重）：返回本次新建的边 id，已存在 = None。
pub fn ensure_edge(session: &mut Session, source: &str, target: &str) -> Option<String> {
    let exists =
        session.data.edges.iter().any(|edge| {
            edge.source == source && edge.target == target && edge.directed != Some(false)
        });
    if exists {
        return None;
    }
    let id = uuid();
    session.data.edges.push(CanvasFileEdge {
        id: id.clone(),
        source: source.to_string(),
        target: target.to_string(),
        source_handle: None,
        target_handle: None,
        directed: None,
        link_mode: None,
        created_at: now_secs(),
    });
    session.touch();
    Some(id)
}

// ===== run 事件 =====

/// 处理一条直写 run 事件（按会话键归属到卡）。
pub async fn on_chat_event(
    plugin: &PluginState,
    ctx: &atelyx_plugin_sdk::Context,
    params: ChatEventParams,
) {
    let Some(node) = node_of_session(&params.session_id).map(str::to_string) else {
        return;
    };
    let mut materialize_id: Option<String> = None;
    plugin.with_session_mut(|session| {
        if session.node(&node).is_none() {
            return;
        }
        match params.event {
            ChatEvent::Message { message, .. } => {
                if upsert_message(session, &node, message) {
                    session.touch();
                }
            }
            ChatEvent::Removed => {
                if remove_message(session, &node, &params.message_id) {
                    session.touch();
                }
            }
            ChatEvent::Finished { .. } => {
                if let Some(chat) = session.view.card_chat.get_mut(&node) {
                    chat.streaming = false;
                    chat.follow_bottom = true;
                }
                session.touch();
                materialize_id = Some(params.message_id.clone());
            }
            ChatEvent::Failed { error } => {
                if let Some(chat) = session.view.card_chat.get_mut(&node) {
                    chat.streaming = false;
                    if !error.is_empty() {
                        chat.error = Some(error);
                    }
                }
            }
            ChatEvent::Titled { .. } => {}
        }
    });
    if let Some(message_id) = materialize_id {
        if plugin.auto_materialize(ctx).await {
            let paths = plugin
                .with_session_mut(|session| materialize(session, &node, &message_id))
                .unwrap_or_default();
            if !paths.is_empty() {
                crate::session::load_refs_for(plugin, ctx, paths).await;
            }
        }
    }
}

/// 中断一张卡正在进行的 run（幂等）。
pub async fn stop(ctx: &atelyx_plugin_sdk::Context, node: &str) {
    let _ = ctx.chat_stop(&session_id(node)).await;
}

// ===== 重生成 / 回滚 / 分支 / 压缩 =====

/// 重生成：截断到最后一条 user 消息（含）并重建其 content 后重新起 run。
/// 截断结果与重建内容立即落卡，重生成不再保留被丢弃的回复。
/// 重生成前的截断：保留到最后一条 user 消息（含）并重建其 content。
/// 返回（截断后的历史，待重发的 user 消息）；没有 user 消息 = None。
fn regen_history(session: &mut Session, node: &str) -> Option<(Vec<Value>, Value)> {
    let messages = messages_of(session, node)?.to_vec();
    let last_user = messages
        .iter()
        .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"))?;
    let mut history = messages[..=last_user].to_vec();
    rebuild_user_content(session, node, &mut history[last_user]);
    let user_message = history.pop()?;
    Some((history, user_message))
}

/// 重生成：截断到最后一条 user 消息（含）并重建其 content 后重新起 run。
/// 截断结果与重建内容立即落卡，重生成不再保留被丢弃的回复。
pub async fn regenerate(
    plugin: &PluginState,
    ctx: &atelyx_plugin_sdk::Context,
    node: &str,
) -> bool {
    let prepared = plugin
        .with_session_mut(|session| {
            if is_streaming(session, node) || locked_by_peer(session, node) {
                return None;
            }
            let (history, user_message) = regen_history(session, node)?;
            let (agent_id, pick) = card_pick(session, node);
            let config = (agent_id, pick, card_effort(session, node));
            replace_messages(session, node, history.clone());
            session.touch();
            if let Some(chat) = session.view.card_chat.get_mut(node) {
                chat.streaming = true;
                chat.error = None;
                chat.follow_bottom = true;
                chat.rollback_confirm = None;
            }
            Some((history, user_message, config))
        })
        .flatten();
    let Some((history, user_message, config)) = prepared else {
        return false;
    };
    if let Err(error) = run(plugin, ctx, node, history, user_message, config).await {
        fail(plugin, node, error);
        return false;
    }
    true
}

/// 重生成前把最后一条 user 消息的 content 按当前引用重新拼装。
fn rebuild_user_content(session: &Session, node: &str, message: &mut Value) {
    let Some(display) = message
        .get("displayContent")
        .and_then(Value::as_str)
        .or_else(|| message.get("content").and_then(Value::as_str))
        .map(str::to_string)
    else {
        return;
    };
    let refs = refs_of(message);
    let hits = refs::scan_hits(&display, &refs);
    let path_block = refs::reference_path_block(&hits);
    let (card_block, image_files) = card_injection(session, &hits, node);
    let mut content = String::with_capacity(path_block.len() + card_block.len() + display.len());
    content.push_str(&path_block);
    content.push_str(&card_block);
    content.push_str(&display);
    message["content"] = Value::String(content);
    if !image_files.is_empty() {
        let Some(array) = message.get_mut("attachments").and_then(Value::as_array_mut) else {
            return;
        };
        for file in image_files {
            if array
                .iter()
                .any(|a| a.get("file").and_then(Value::as_str) == Some(file.as_str()))
            {
                continue;
            }
            let ext = file
                .rsplit_once('.')
                .map(|(_, e)| e.to_ascii_lowercase())
                .unwrap_or_default();
            array.push(json!({ "kind": "image", "mime": image_mime(&ext), "file": file }));
        }
    }
}

/// 从消息的 refs 字段还原引用条目。
fn refs_of(message: &Value) -> Vec<RefEntry> {
    message
        .get("refs")
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(|entry| {
                    let label = entry.get("label").and_then(Value::as_str)?.to_string();
                    let kind = entry
                        .get("refKind")
                        .and_then(Value::as_str)
                        .and_then(RefKind::parse_key)?;
                    let node_id = entry
                        .get("nodeId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let path = entry
                        .get("path")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| node_id.clone())?;
                    Some(RefEntry {
                        path,
                        label,
                        kind,
                        node_id,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 回到此处：保留到指定消息（含），更后面的消息丢弃。
pub fn rollback(session: &mut Session, node: &str, message_id: &str) -> bool {
    if is_streaming(session, node) || locked_by_peer(session, node) {
        return false;
    }
    let Some(messages) = messages_of(session, node) else {
        return false;
    };
    let Some(index) = messages
        .iter()
        .position(|m| m.get("id").and_then(Value::as_str) == Some(message_id))
    else {
        return false;
    };
    if index + 1 >= messages.len() {
        return false;
    }
    let kept = messages[..=index].to_vec();
    replace_messages(session, node, kept);
    session.touch();
    true
}

/// 「回到此处」单击：首点只进入待确认态，同一消息再点才截断。
/// 返回是否真的截断了消息列表。
pub fn rollback_click(session: &mut Session, node: &str, message_id: &str) -> bool {
    let confirmed = session
        .view
        .card_chat
        .get(node)
        .and_then(|chat| chat.rollback_confirm.as_deref())
        == Some(message_id);
    if confirmed {
        return confirm_rollback(session, node, message_id);
    }
    if let Some(chat) = session.view.card_chat.get_mut(node) {
        chat.rollback_confirm = Some(message_id.to_string());
    }
    false
}

/// 「确认回滚？」：直接截断并清掉待确认态。
pub fn confirm_rollback(session: &mut Session, node: &str, message_id: &str) -> bool {
    if let Some(chat) = session.view.card_chat.get_mut(node) {
        chat.rollback_confirm = None;
    }
    rollback(session, node, message_id)
}

/// 取消待确认的回滚。
pub fn cancel_rollback(session: &mut Session, node: &str) {
    if let Some(chat) = session.view.card_chat.get_mut(node) {
        chat.rollback_confirm = None;
    }
}

/// 分支：把到指定消息为止的内容复制成右侧新对话卡。
pub fn branch(session: &mut Session, node: &str, message_id: &str) -> Option<String> {
    if locked_by_peer(session, node) {
        return None;
    }
    let source = session.node(node)?.clone();
    let messages = messages_of(session, node)?.to_vec();
    let index = messages
        .iter()
        .position(|m| m.get("id").and_then(Value::as_str) == Some(message_id))?;
    let kept = messages[..=index].to_vec();
    let (w, _) = display_size(&source);
    let spot = find_free_spot(
        &session.data.nodes,
        (source.x + w + CARD_GAP, source.y),
        (520.0, 420.0),
        SPOT_STEP,
    );
    let id = uuid();
    let mut data = source.data.clone();
    data["messages"] = Value::Array(kept);
    session.data.nodes.push(CanvasFileNode {
        id: id.clone(),
        node_type: "conversation".to_string(),
        x: spot.0,
        y: spot.1,
        width: Some(520.0),
        height: Some(420.0),
        data,
    });
    session.data.edges.push(CanvasFileEdge {
        id: uuid(),
        source: node.to_string(),
        target: id.clone(),
        source_handle: None,
        target_handle: None,
        directed: None,
        link_mode: None,
        created_at: now_secs(),
    });
    session.view.card_chat.insert(
        id.clone(),
        CardChat {
            follow_bottom: true,
            ..Default::default()
        },
    );
    session.touch();
    Some(id)
}

/// 压缩卡会话：摘要写 `data.compaction`，消息本体不动。
pub async fn compact(plugin: &PluginState, ctx: &atelyx_plugin_sdk::Context, node: &str) -> bool {
    let too_short = plugin
        .with_session(|session| {
            messages_of(session, node).is_some_and(|messages| messages.len() < 2)
        })
        .unwrap_or(false);
    if too_short {
        plugin.with_session_mut(|session| {
            if let Some(chat) = session.view.card_chat.get_mut(node) {
                chat.error = Some("对话内容太少，无需压缩".to_string());
            }
        });
        return false;
    }
    let Some((messages, compaction, agent_id, pick)) = plugin
        .with_session_mut(|session| {
            let chat = session.view.card_chat.get(node)?;
            if chat.streaming || chat.compacting || locked_by_peer(session, node) {
                return None;
            }
            let messages = messages_of(session, node)?.to_vec();
            let compaction = session.node(node)?.data.get("compaction").cloned();
            let (agent_id, pick) = card_pick(session, node);
            session.view.card_chat.get_mut(node)?.compacting = true;
            Some((messages, compaction, agent_id, pick))
        })
        .flatten()
    else {
        return false;
    };
    let (model, provider_id) = match pick {
        Some((provider, model)) => (Some(model), Some(provider)),
        None => (None, None),
    };
    let params = ChatDirectCompactParams {
        session_id: session_id(node),
        messages,
        compaction,
        agent_id,
        model,
        provider_id,
    };
    let result: Result<Value, String> = match serde_json::to_value(params) {
        Ok(value) => ctx
            .call_raw(method::ai::CHAT_COMPACT_DIRECT, value)
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    plugin.with_session_mut(|session| {
        if let Some(chat) = session.view.card_chat.get_mut(node) {
            chat.compacting = false;
        }
        match result {
            Ok(value) => {
                let summary = value
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if summary.trim().is_empty() {
                    if let Some(chat) = session.view.card_chat.get_mut(node) {
                        chat.error = Some("压缩失败：摘要为空".to_string());
                    }
                    return;
                }
                let last_id = messages_of(session, node)
                    .and_then(|m| m.last())
                    .and_then(|m| m.get("id").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                let count = messages_of(session, node).map(<[Value]>::len).unwrap_or(0);
                if let Some(card) = session.node_mut(node) {
                    card.data["compaction"] = json!({
                        "summary": summary,
                        "upToMessageId": last_id,
                        "messageCount": count,
                        "createdAt": now_ms(),
                    });
                }
                session.touch();
            }
            Err(error) => {
                if let Some(chat) = session.view.card_chat.get_mut(node) {
                    chat.error = Some(format!("压缩失败：{error}"));
                }
            }
        }
    });
    true
}

// ===== 产出落卡 =====

/// 落卡产物：最终正文文本卡、输出文件卡、输入文件卡。
enum Material {
    OutputText(String),
    OutputFile(String),
    InputFile(String),
}

/// 结算一条回复：正文 / 文件引用落成新卡，返回需要补读的引用路径。
fn materialize(session: &mut Session, node_id: &str, message_id: &str) -> Vec<String> {
    let Some(node) = session.node(node_id) else {
        return Vec::new();
    };
    let Some(message) = node
        .data
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|m| m.get("id").and_then(Value::as_str) == Some(message_id))
        })
        .cloned()
    else {
        return Vec::new();
    };
    let content = message.get("content").and_then(Value::as_str).unwrap_or("");
    let readable = !content.trim().is_empty() && !content.starts_with("[错误]");
    let mut materials = Vec::new();
    if let Some(steps) = message.get("steps").and_then(Value::as_array) {
        for step in steps {
            let Some(run) = step.get("run") else { continue };
            let name = run.get("name").and_then(Value::as_str).unwrap_or("");
            let status = run.get("status").and_then(Value::as_str).unwrap_or("");
            if status != "done" {
                continue;
            }
            let Some(path) = run
                .get("args")
                .and_then(|a| a.get("path"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            match name {
                "write_file" | "append_file" | "edit_file" => {
                    materials.push(Material::OutputFile(path.to_string()))
                }
                "read_file" => materials.push(Material::InputFile(path.to_string())),
                _ => {}
            }
        }
    }
    if readable {
        materials.push(Material::OutputText(content.to_string()));
    }
    apply_materials(session, node_id, materials)
}

fn apply_materials(
    session: &mut Session,
    conversation: &str,
    materials: Vec<Material>,
) -> Vec<String> {
    let Some(conv) = session.node(conversation).cloned() else {
        return Vec::new();
    };
    let (cw, _) = display_size(&conv);
    let mut changed = false;
    let mut paths: Vec<String> = Vec::new();
    for material in materials {
        match material {
            Material::OutputText(text) => {
                if session.data.nodes.iter().any(|n| {
                    n.node_type == "text"
                        && n.data.get("file").and_then(Value::as_str).is_none()
                        && n.data.get("bodyMd").and_then(Value::as_str) == Some(text.as_str())
                }) {
                    continue;
                }
                let spot = find_free_spot(
                    &session.data.nodes,
                    (conv.x + cw + CARD_GAP, conv.y),
                    (TEXT_CARD_W, TEXT_CARD_H),
                    SPOT_STEP,
                );
                let id = uuid();
                session.data.nodes.push(CanvasFileNode {
                    id: id.clone(),
                    node_type: "text".to_string(),
                    x: spot.0,
                    y: spot.1,
                    width: Some(TEXT_CARD_W),
                    height: Some(TEXT_CARD_H),
                    data: json!({ "title": first_line_prefix(&text, 16), "bodyMd": text }),
                });
                session.data.edges.push(plain_edge(conversation, &id));
                changed = true;
            }
            Material::OutputFile(path) => {
                if let Some(existing) = find_file_card(session, &path) {
                    if !has_edge(session, conversation, &existing) {
                        session.data.edges.push(plain_edge(conversation, &existing));
                        changed = true;
                    }
                    continue;
                }
                let Some(card) = file_card(&path) else {
                    continue;
                };
                let (node_type, w, h, data) = card;
                let spot = find_free_spot(
                    &session.data.nodes,
                    (conv.x + cw + CARD_GAP, conv.y),
                    (w, h),
                    SPOT_STEP,
                );
                let id = uuid();
                session.data.nodes.push(CanvasFileNode {
                    id: id.clone(),
                    node_type: node_type.to_string(),
                    x: spot.0,
                    y: spot.1,
                    width: Some(w),
                    height: Some(h),
                    data,
                });
                session.data.edges.push(plain_edge(conversation, &id));
                paths.push(path);
                changed = true;
            }
            Material::InputFile(path) => {
                if let Some(existing) = find_file_card(session, &path) {
                    if !has_edge(session, &existing, conversation) {
                        session.data.edges.push(plain_edge(&existing, conversation));
                        changed = true;
                    }
                    continue;
                }
                let Some(card) = file_card(&path) else {
                    continue;
                };
                let (node_type, w, h, data) = card;
                let spot = find_free_spot(
                    &session.data.nodes,
                    (conv.x - w - CARD_GAP, conv.y),
                    (w, h),
                    SPOT_STEP,
                );
                let id = uuid();
                session.data.nodes.push(CanvasFileNode {
                    id: id.clone(),
                    node_type: node_type.to_string(),
                    x: spot.0,
                    y: spot.1,
                    width: Some(w),
                    height: Some(h),
                    data,
                });
                session.data.edges.push(plain_edge(&id, conversation));
                paths.push(path);
                changed = true;
            }
        }
    }
    if changed {
        session.touch();
    }
    paths
}

fn plain_edge(source: &str, target: &str) -> CanvasFileEdge {
    CanvasFileEdge {
        id: uuid(),
        source: source.to_string(),
        target: target.to_string(),
        source_handle: None,
        target_handle: None,
        directed: None,
        link_mode: None,
        created_at: now_secs(),
    }
}

fn find_file_card(session: &Session, path: &str) -> Option<String> {
    session
        .data
        .nodes
        .iter()
        .find(|n| {
            n.data.get("file").and_then(Value::as_str) == Some(path)
                && matches!(n.node_type.as_str(), "text" | "table" | "media")
        })
        .map(|n| n.id.clone())
}

fn has_edge(session: &Session, source: &str, target: &str) -> bool {
    session
        .data
        .edges
        .iter()
        .any(|e| e.source == source && e.target == target && e.directed != Some(false))
}

/// 文件 → 卡类型与默认尺寸（文本卡引用 / 表格薄卡 / 媒体卡）。
fn file_card(path: &str) -> Option<(&'static str, f64, f64, Value)> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let title = crate::model::file_title(path);
    match ext.as_str() {
        "md" => Some((
            "text",
            TEXT_CARD_W,
            TEXT_CARD_H,
            json!({ "title": title, "file": path }),
        )),
        "atb" => Some((
            "table",
            300.0,
            110.0,
            json!({ "title": title, "file": path }),
        )),
        "atlx" => None,
        _ if image_ext_of(name).is_some() => Some((
            "media",
            260.0,
            240.0,
            json!({
                "file": path,
                "kind": "image",
                "mime": image_mime(&ext),
                "name": name,
            }),
        )),
        _ => Some((
            "media",
            260.0,
            240.0,
            json!({ "file": path, "kind": "file", "mime": "", "name": name }),
        )),
    }
}

/// 正文首行前 N 字（落卡标题）。
fn first_line_prefix(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    line.chars().take(max).collect()
}

/// 毫秒时间戳（消息 createdAt）。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

// ===== `#` 候选 =====

/// 草稿里已登记引用的标签区间（字节）：每条取首个实例，重叠让位。
pub fn marks(chat: &CardChat) -> Vec<(usize, usize)> {
    let mut found: Vec<(usize, usize)> = chat
        .mention_refs
        .iter()
        .filter_map(|entry| {
            let tag = entry.tag_text();
            chat.draft
                .find(&tag)
                .map(|start| (start, start + tag.len()))
        })
        .collect();
    found.sort_by_key(|(start, _)| *start);
    let mut out = Vec::new();
    let mut prev_end = 0usize;
    for (start, end) in found {
        if start >= prev_end {
            prev_end = end;
            out.push((start, end));
        }
    }
    out
}

/// 按草稿与光标刷新 `#` 候选（画布卡在前，其后为仓库文件与目录）。
pub fn refresh_picker(plugin: &PluginState, session: &mut Session, node: &str) {
    let (files, dirs) = plugin.vault_choices();
    let Some(chat) = session.view.card_chat.get_mut(node) else {
        return;
    };
    let caret = chat.caret.min(chat.draft.len());
    chat.mention_picker = None;
    let Some(at) = chat.draft[..caret].rfind('#') else {
        return;
    };
    if !refs::is_trigger_position(&chat.draft[..at]) {
        return;
    }
    let query = &chat.draft[at + 1..caret];
    if query
        .chars()
        .any(|c| c.is_whitespace() || c == '#' || c == '：' || c == ':')
    {
        return;
    }
    let needle = query.to_lowercase();
    let mut candidates: Vec<crate::model::MentionTarget> = Vec::new();
    for card in &session.data.nodes {
        if card.id == node || !matches!(card.node_type.as_str(), "text" | "media" | "table") {
            continue;
        }
        let title = card_ref_title(card);
        if needle.is_empty() || title.to_lowercase().contains(&needle) {
            candidates.push(crate::model::MentionTarget::Card {
                node_id: card.id.clone(),
            });
        }
    }
    for (path, is_dir) in dirs
        .iter()
        .map(|d| (d, true))
        .chain(files.iter().map(|f| (f, false)))
    {
        if needle.is_empty() || path.to_lowercase().contains(&needle) {
            candidates.push(crate::model::MentionTarget::Vault {
                rel: path.clone(),
                is_dir,
            });
        }
        if candidates.len() >= 50 {
            break;
        }
    }
    if candidates.is_empty() {
        return;
    }
    chat.mention_picker = Some(crate::model::MentionPicker {
        at,
        selected: 0,
        candidates,
    });
}

/// 点选候选：替换 `#查询` 为完整标签，登记引用与连线。
pub fn commit_mention(session: &mut Session, node: &str, index: usize) -> bool {
    let Some(chat) = session.view.card_chat.get(node) else {
        return false;
    };
    let Some(picker) = chat.mention_picker.clone() else {
        return false;
    };
    let Some(candidate) = picker.candidates.get(index).cloned() else {
        return false;
    };
    let (tag, entry) = match candidate {
        // 卡片引用统一由 append_card_ref 生成标签、登记引用与连线
        crate::model::MentionTarget::Card { node_id } => {
            return crate::actions::append_card_ref(session, &node_id, node).is_some();
        }
        crate::model::MentionTarget::Vault { rel, is_dir } => {
            let Some(kind) = (if is_dir {
                Some(RefKind::Dir)
            } else {
                refs::kind_of_file(&rel)
            }) else {
                return false;
            };
            let existing = session
                .view
                .card_chat
                .get(node)
                .and_then(|chat| chat.mention_refs.iter().find(|entry| entry.path == rel))
                .map(|entry| entry.label.clone());
            let label = match existing {
                Some(label) => label,
                None => {
                    let taken: Vec<(&str, &str)> = session
                        .view
                        .card_chat
                        .get(node)
                        .map(|chat| {
                            chat.mention_refs
                                .iter()
                                .map(|entry| (entry.path.as_str(), entry.label.as_str()))
                                .collect()
                        })
                        .unwrap_or_default();
                    refs::make_label(&rel, &taken)
                }
            };
            (
                format!("#{}：{label} ", kind.prefix()),
                RefEntry {
                    path: rel,
                    label,
                    kind,
                    node_id: None,
                },
            )
        }
    };
    let Some(chat) = session.view.card_chat.get_mut(node) else {
        return false;
    };
    let caret = chat.caret.min(chat.draft.len());
    chat.caret = crate::actions::insert_tag(&mut chat.draft, picker.at, caret, &tag);
    if !chat
        .mention_refs
        .iter()
        .any(|existing| existing.path == entry.path)
    {
        chat.mention_refs.push(entry);
    }
    chat.mention_picker = None;
    true
}

/// 关闭/切文件：中断所有卡的活跃 run。
pub async fn detach_all(plugin: &PluginState, ctx: &atelyx_plugin_sdk::Context) {
    let ids: Vec<String> = plugin
        .with_session(|session| {
            session
                .view
                .card_chat
                .iter()
                .filter(|(_, chat)| chat.streaming)
                .map(|(id, _)| session_id(id))
                .collect()
        })
        .unwrap_or_default();
    for id in ids {
        let _ = ctx.chat_stop(&id).await;
    }
}

/// 单卡运行态清理（删卡 / 删选中）。
pub fn detach_node(_plugin: &PluginState, ctx: &atelyx_plugin_sdk::Context, node: &str) {
    let ctx = ctx.clone();
    let id = session_id(node);
    tokio::spawn(async move {
        let _ = ctx.chat_stop(&id).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_roundtrip() {
        assert_eq!(node_of_session(&session_id("node-1")), Some("node-1"));
        assert_eq!(node_of_session("chat-1"), None);
    }

    #[test]
    fn media_session_id_is_path_safe() {
        assert_eq!(media_session_id("a/b:c d"), "canvas-a-b-c-d");
        let long = media_session_id(&"x".repeat(300));
        assert_eq!(long.len(), "canvas-".len() + 128);
    }

    #[test]
    fn file_cards_cover_known_types_and_skip_canvas() {
        assert_eq!(file_card("a/b.md").unwrap().0, "text");
        assert_eq!(file_card("a/b.atb").unwrap().0, "table");
        assert_eq!(file_card("a/b.png").unwrap().0, "media");
        assert_eq!(file_card("a/b.zip").unwrap().0, "media");
        assert!(file_card("a/b.atlx").is_none());
    }

    #[test]
    fn image_extensions_are_restricted() {
        assert_eq!(image_ext_of("a.PNG").as_deref(), Some("png"));
        assert!(image_ext_of("a.txt").is_none());
        assert_eq!(image_ext_of("a.jpg").as_deref(), Some("jpg"));
        assert_eq!(image_ext_of("a.jpeg").as_deref(), Some("jpeg"));
        assert!(image_ext_of("a.bmp").is_none());
        assert_eq!(image_mime("jpeg"), "image/jpeg");
    }

    #[test]
    fn upsert_replaces_by_id_and_ignores_identical() {
        let mut session = Session::new(
            "a.atlx".into(),
            atelyx_format::canvas::CanvasFile {
                schema: atelyx_format::canvas::CANVAS_SCHEMA.into(),
                id: "c".into(),
                title: "a".into(),
                nodes: vec![CanvasFileNode {
                    id: "n".into(),
                    node_type: "conversation".into(),
                    x: 0.0,
                    y: 0.0,
                    width: None,
                    height: None,
                    data: json!({}),
                }],
                edges: Vec::new(),
                created_at: 0,
                updated_at: 0,
            },
            1,
        );
        let message = json!({ "id": "m1", "role": "user", "content": "x" });
        assert!(upsert_message(&mut session, "n", message.clone()));
        assert!(!upsert_message(&mut session, "n", message));
        assert_eq!(messages_of(&session, "n").unwrap().len(), 1);
        assert!(remove_message(&mut session, "n", "m1"));
        assert!(messages_of(&session, "n").unwrap().is_empty());
    }

    #[test]
    fn picker_marks_only_committed_tags() {
        let chat = CardChat {
            draft: "#笔记：a.md 和 #笔记：a.md".into(),
            mention_refs: vec![RefEntry {
                path: "a.md".into(),
                label: "a.md".into(),
                kind: RefKind::Note,
                node_id: None,
            }],
            ..Default::default()
        };
        let marks = marks(&chat);
        assert_eq!(marks.len(), 1);
    }

    fn canvas_with(nodes: Vec<CanvasFileNode>) -> atelyx_format::canvas::CanvasFile {
        atelyx_format::canvas::CanvasFile {
            schema: atelyx_format::canvas::CANVAS_SCHEMA.into(),
            id: "c".into(),
            title: "a".into(),
            nodes,
            edges: Vec::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn conversation(id: &str, messages: Value) -> CanvasFileNode {
        CanvasFileNode {
            id: id.into(),
            node_type: "conversation".into(),
            x: 0.0,
            y: 0.0,
            width: None,
            height: None,
            data: json!({ "messages": messages }),
        }
    }

    fn conversation_session(messages: Value) -> Session {
        let mut session = Session::new(
            "a.atlx".into(),
            canvas_with(vec![conversation("n", messages)]),
            1,
        );
        sync_card_chats(&mut session);
        session
    }

    /// 节点与消息 id 都是带连字符的 UUID：编码不能依赖 `-` 分段。
    #[test]
    fn message_action_ids_roundtrip_uuid_shaped_ids() {
        let node = "6f1a2b3c-4d5e-4f60-8a9b-0c1d2e3f4a5b";
        let message = "9f0e2f5c-1a2b-4c3d-8e9f-0a1b2c3d4e5f";
        for action in [
            MessageAction::Copy,
            MessageAction::Rollback,
            MessageAction::RollbackConfirm,
            MessageAction::Branch,
            MessageAction::Regen,
        ] {
            let id = message_action_id(action, node, Some(message));
            assert_eq!(
                parse_message_action(&id),
                Some((action, node, Some(message)))
            );
        }
        let cancel = message_action_id(MessageAction::CancelRollback, node, None);
        assert_eq!(
            parse_message_action(&cancel),
            Some((MessageAction::CancelRollback, node, None))
        );
        assert_eq!(parse_message_action("canvas-msg-copy:"), None);
        assert_eq!(parse_message_action("canvas-bottom-n"), None);
    }

    #[test]
    fn regen_truncates_after_last_user_and_lands_on_card() {
        let messages = json!([
            { "id": "m1", "role": "user", "content": "一" },
            { "id": "m2", "role": "assistant", "content": "答一" },
            { "id": "m3", "role": "user", "content": "二" },
            { "id": "m4", "role": "assistant", "content": "答二" }
        ]);
        let mut session = conversation_session(messages);
        let (history, user_message) = regen_history(&mut session, "n").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(
            user_message.get("id").and_then(Value::as_str),
            Some("m3")
        );
        replace_messages(&mut session, "n", history);
        assert_eq!(messages_of(&session, "n").unwrap().len(), 2);
    }

    #[test]
    fn rollback_requires_confirmation_and_cancel_clears_it() {
        let messages = json!([
            { "id": "m1", "role": "user", "content": "一" },
            { "id": "m2", "role": "assistant", "content": "答" },
            { "id": "m3", "role": "user", "content": "二" }
        ]);
        let mut session = conversation_session(messages);
        assert!(!rollback_click(&mut session, "n", "m2"));
        let confirm = session
            .view
            .card_chat
            .get("n")
            .and_then(|chat| chat.rollback_confirm.clone());
        assert_eq!(confirm.as_deref(), Some("m2"));
        assert_eq!(messages_of(&session, "n").unwrap().len(), 3);

        cancel_rollback(&mut session, "n");
        let confirm = session
            .view
            .card_chat
            .get("n")
            .and_then(|chat| chat.rollback_confirm.clone());
        assert!(confirm.is_none());

        assert!(!rollback_click(&mut session, "n", "m2"));
        assert!(rollback_click(&mut session, "n", "m2"));
        assert_eq!(messages_of(&session, "n").unwrap().len(), 2);
        let confirm = session
            .view
            .card_chat
            .get("n")
            .and_then(|chat| chat.rollback_confirm.clone());
        assert!(confirm.is_none());
    }
}
