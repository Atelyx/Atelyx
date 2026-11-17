//! 画布对话卡编排：每卡草稿 / 引用 / 附件与 run 直写（消息真源在节点 `data.messages`）。
//! run 循环仍由 core `AgentHub` 执行；本模块只组装请求、应用快照与落卡。

use std::collections::{HashMap, HashSet};

use iced::Task;
use serde_json::{json, Value};

use atelyx_core::ai::{
    agent::{RunConfig, RunEvent, RunPersist},
    canvas_refs::{self, CardContent, CardSource},
    chat_send::{self, RefsShape},
    refs::{self as ai_refs, RefEntry, RefHit},
    stream::ReasoningEffort,
};

use crate::canvas_view::{file_title, CanvasMsg, CanvasSession, RefContent};
use crate::chat_actions::flow::{self, ChatAction, ChatStore, SendReady};
use crate::chat_actions::{self, MentionPicker, MentionTarget, PendingImage};
use crate::text_preedit;
use crate::{App, Message, RepoKind};

/// 画布卡会话 id 前缀（与面板会话 id 区分；Hub 与 run 事件按它路由）。
const CANVAS_SESSION_PREFIX: &str = "canvas:";
/// 产出落卡左右间距与纵向搜索步长（世界坐标）。
const CARD_GAP: f64 = 40.0;
const SPOT_STEP: f64 = 24.0;
/// 新文本卡默认尺寸（世界坐标）。
const TEXT_CARD_W: f64 = 320.0;
const TEXT_CARD_H: f64 = 240.0;

/// 卡片运行态消息（View → 编排）。
#[derive(Debug, Clone)]
pub enum CanvasChatMsg {
    Input { node: String, event: crate::chat_input::InputEvent },
    MentionPick { node: String, index: usize },
    TriggerMention(String),
    PickFiles(String),
    FilesPicked { node: String, paths: Vec<std::path::PathBuf> },
    RemoveAttachment { node: String, index: usize },
    Send(String),
    Stop(String),
    Regenerate(String),
    Rollback { node: String, message_id: String },
    RollbackCancel(String),
    Branch { node: String, message_id: String },
    ClearError(String),
    FlowScrolled { node: String, y: f32 },
    OpenInPanel(String),
    /// 卡 Agent 选择（写节点 `data.agentId`；随 `.atlx` 持久化，面板绑定态同源）。
    AgentPick { node: String, agent: Option<String> },
    /// 卡模型选择（`providerId` + `model` 成对写；None = 移除回落仓库默认）。
    ModelPick { node: String, pick: Option<(String, String)> },
    /// 卡推理等级选择（写节点 `data.reasoningEffort`）。
    EffortPick { node: String, effort: Option<String> },
    /// 卡会话压缩（摘要写节点 `data.compaction`，消息本体不动）。
    Compact(String),
    /// 卡消息流一键回底。
    ScrollBottom(String),
    /// 空间模式 Agent 提示词取回（续跑发送）。
    PromptReady { gen: u64, node: String, result: Result<String, String> },
    /// 空间模式重生成的提示词取回。
    RegenPromptReady { gen: u64, node: String, result: Result<String, String> },
    /// 附件落盘回执（续跑发送）。
    Ready {
        gen: u64,
        plan: Box<SendPlan>,
        result: Result<Vec<Value>, (Vec<PendingImage>, String)>,
    },
}

/// 单张对话卡的运行态（草稿 / 引用 / 附件 / 流式 / 错误）。
#[derive(Default)]
pub struct CardChat {
    pub draft: crate::chat_input::Draft,
    pub mention_refs: Vec<ai_refs::RefEntry>,
    pub mention_picker: Option<MentionPicker>,
    pub press_inside: bool,
    pub pending_images: Vec<PendingImage>,
    pub streaming: bool,
    pub error: Option<String>,
    pub follow_bottom: bool,
    /// 回滚二段确认（messageId）。
    pub rollback_confirm: Option<String>,
    /// 卡会话压缩在途（摘要回执前不再接受新压缩）。
    pub compacting: bool,
    /// 压缩请求时刻的消息锚点（末条 id + 条数）：回执沿用它，期间新增消息不并入摘要。
    compact_anchor: Option<(String, usize)>,
    /// 空间模式提示词预取在途（回执后续跑发送）。
    pending_send: Option<SendPlan>,
    /// 空间模式重生成的待跑历史（提示词取回后启动）。
    pending_regen: Option<Vec<Value>>,
}

/// 发送草稿来源（失败时归还到原输入面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOrigin {
    Card,
    Panel,
}

/// 两段式发送的续跑包（提示词与附件落盘回执后消费）。
#[derive(Debug, Clone)]
pub struct SendPlan {
    node: String,
    display: String,
    refs: Vec<ai_refs::RefEntry>,
    images: Vec<PendingImage>,
    agent_id: Option<String>,
    model_pick: Option<(String, String)>,
    effort: Option<ReasoningEffort>,
    origin: SendOrigin,
    /// 配置在附件落盘前解析（失败即早退，不产生半发消息）。
    config: Option<RunConfig>,
}

impl SendPlan {
    /// 附件临时区目录身份：会话 id 只允许字母数字与连字符（节点 id 多数是 uuid）。
    fn media_session_id(&self) -> String {
        let id: String = self
            .node
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
            .take(128)
            .collect();
        format!("canvas-{id}")
    }
}

/// 画布卡会话 id（Hub run 与面板绑定共用的身份）。
pub fn session_id(node_id: &str) -> String {
    format!("{CANVAS_SESSION_PREFIX}{node_id}")
}

/// 会话 id → 卡节点 id（非画布会话 = None）。
pub fn node_of_session(session_id: &str) -> Option<&str> {
    session_id.strip_prefix(CANVAS_SESSION_PREFIX)
}

// ===== 数据读写 =====

/// 卡的显示标题：内嵌 title → 首条 user 消息前缀 → 「对话」。
pub fn card_title(node: &atelyx_core::vault::CanvasFileNode) -> String {
    if let Some(title) = node.data.get("title").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        return title.to_string();
    }
    node.data
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|ms| ms.iter().find(|m| m["role"] == json!("user")))
        .and_then(|m| {
            m.get("displayContent")
                .and_then(Value::as_str)
                .or_else(|| m.get("content").and_then(Value::as_str))
        })
        .map(|s| first_line_prefix(s, 16))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "对话".to_string())
}

fn first_line_prefix(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    line.chars().take(max).collect()
}

/// 会话真源里的消息（画布卡：节点 data.messages）。
pub fn messages_of<'a>(app: &'a App, session_id: &str) -> Option<&'a [Value]> {
    let node_id = node_of_session(session_id)?;
    let node = app.canvas.as_ref()?.data.nodes.iter().find(|n| n.id == node_id)?;
    node.data.get("messages").and_then(Value::as_array).map(Vec::as_slice)
}

fn node<'a>(app: &'a App, node_id: &str) -> Option<&'a atelyx_core::vault::CanvasFileNode> {
    app.canvas.as_ref()?.data.nodes.iter().find(|n| n.id == node_id)
}

fn card_chat<'a>(app: &'a App, node_id: &str) -> Option<&'a CardChat> {
    app.canvas.as_ref()?.view.card_chat.get(node_id)
}

/// 被对端独占编辑的卡不接受任何消息写入（发送 / 重生成 / 回滚 / 分支同源守卫；
/// UI 已按只读呈现，此为锁传播窗口的兜底）。
fn locked_by_peer(app: &App, node_id: &str) -> bool {
    matches!(
        crate::space_actions::conversation_lock_holder(app, node_id),
        crate::space_actions::LockHolder::Peer { .. }
    )
}

/// 当前是否有活跃 run（Hub 注册表为准）或卡片在流式态。
pub fn is_streaming(app: &App, node_id: &str) -> bool {
    card_chat(app, node_id).is_some_and(|c| c.streaming)
        || chat_actions::hub().is_running(&session_id(node_id))
}

/// 画布会话离场（切文件 / 切仓库 / 关标签）：停掉全部卡内 run，并解绑面板的同会话视图。
pub fn detach_canvas(app: &mut App) {
    if let Some(session) = app.canvas.as_ref() {
        for id in session.view.card_chat.keys() {
            chat_actions::hub().stop(&session_id(id));
        }
    }
    if app
        .chat
        .active
        .as_deref()
        .and_then(node_of_session)
        .is_some()
    {
        app.chat.active = None;
        app.chat.selection = None;
        app.chat.rollback_confirm = None;
    }
}

/// 单卡离场（删除卡）：停该卡 run，面板绑定此卡则解绑。
pub fn detach_node(app: &mut App, node_id: &str) {
    chat_actions::hub().stop(&session_id(node_id));
    if app.chat.active.as_deref() == Some(session_id(node_id).as_str()) {
        app.chat.active = None;
        app.chat.selection = None;
        app.chat.rollback_confirm = None;
    }
}

/// 对齐卡运行态与节点集合：新增对话节点补默认态，已删节点停 run 并清态。
pub fn sync_card_chats(app: &mut App) {
    let Some(session) = app.canvas.as_mut() else { return };
    let live: Vec<String> = session
        .data
        .nodes
        .iter()
        .filter(|n| n.node_type == "conversation")
        .map(|n| n.id.clone())
        .collect();
    let stale: Vec<String> = session
        .view
        .card_chat
        .keys()
        .filter(|id| !live.iter().any(|x| x == *id))
        .cloned()
        .collect();
    for id in stale {
        chat_actions::hub().stop(&session_id(&id));
        session.view.card_chat.remove(&id);
    }
    for id in live {
        session.view.card_chat.entry(id).or_default();
    }
    // 浮层槽位：新节点追加、末尾删除回收；中间删除保留空槽，后续卡的下标不前移
    let ids: Vec<String> = session.data.nodes.iter().map(|n| n.id.clone()).collect();
    sync_overlay_slots(&mut session.view.overlay_slots, &ids);
}

/// 浮层槽位对齐（纯逻辑）：末尾已删槽位回收，中间已删槽位保留，新节点追加到末尾。
/// 每次画布消息都会调用，按集合比较保持 O(n)，不做嵌套线性查找。
fn sync_overlay_slots(slots: &mut Vec<String>, ids: &[String]) {
    let live: HashSet<&str> = ids.iter().map(String::as_str).collect();
    while slots.last().is_some_and(|id| !live.contains(id.as_str())) {
        slots.pop();
    }
    let existing: HashSet<&str> = slots.iter().map(String::as_str).collect();
    let missing: Vec<String> = ids
        .iter()
        .filter(|id| !existing.contains(id.as_str()))
        .cloned()
        .collect();
    slots.extend(missing);
}

/// 清掉全部卡输入焦点（面板输入取得焦点 / 画布空白按压时调用）。
pub fn clear_card_focus(app: &mut App) {
    if let Some(session) = app.canvas.as_mut() {
        for chat in session.view.card_chat.values_mut() {
            chat.draft.focused = false;
        }
    }
}

// ===== 消息入口 =====

pub fn on_msg(app: &mut App, msg: CanvasChatMsg) -> Task<Message> {
    match msg {
        CanvasChatMsg::Input { node, event } => on_input(app, &node, event),
        CanvasChatMsg::MentionPick { node, index } => {
            if let Some(chat) = app.canvas.as_mut().and_then(|s| s.view.card_chat.get_mut(&node)) {
                chat.press_inside = true;
                chat.draft.focused = true;
            }
            app.chat.input.focused = false;
            commit_mention(app, &node, index);
            Task::none()
        }
        CanvasChatMsg::TriggerMention(node) => {
            app.dismiss_popup();
            trigger_mention(app, &node)
        }
        CanvasChatMsg::PickFiles(node) => {
            app.dismiss_popup();
            pick_files(app, node)
        }
        CanvasChatMsg::FilesPicked { node, paths } => {
            add_attachments(app, &node, paths);
            Task::none()
        }
        CanvasChatMsg::RemoveAttachment { node, index } => {
            if let Some(chat) = app.canvas.as_mut().and_then(|s| s.view.card_chat.get_mut(&node)) {
                if index < chat.pending_images.len() {
                    chat.pending_images.remove(index);
                }
            }
            Task::none()
        }
        CanvasChatMsg::Send(node) => send(app, &node),
        CanvasChatMsg::Stop(node) => {
            chat_actions::hub().stop(&session_id(&node));
            Task::none()
        }
        CanvasChatMsg::Regenerate(node) => regenerate(app, &node),
        CanvasChatMsg::Rollback { node, message_id } => {
            let confirm = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(&node))
                .map(|chat| {
                    if chat.rollback_confirm.as_deref() == Some(message_id.as_str()) {
                        chat.rollback_confirm = None;
                        true
                    } else {
                        chat.rollback_confirm = Some(message_id.clone());
                        false
                    }
                })
                .unwrap_or(false);
            if confirm {
                rollback(app, &node, &message_id);
            }
            Task::none()
        }
        CanvasChatMsg::RollbackCancel(node) => {
            if let Some(chat) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(&node))
            {
                chat.rollback_confirm = None;
            }
            Task::none()
        }
        CanvasChatMsg::Branch { node, message_id } => branch(app, &node, &message_id),
        CanvasChatMsg::ClearError(node) => {
            clear_error(app, &node);
            Task::none()
        }
        CanvasChatMsg::FlowScrolled { node, y } => {
            if let Some(chat) = app.canvas.as_mut().and_then(|s| s.view.card_chat.get_mut(&node)) {
                chat.follow_bottom = y >= 0.995;
            }
            Task::none()
        }
        CanvasChatMsg::OpenInPanel(node) => {
            open_in_panel(app, &node);
            Task::none()
        }
        CanvasChatMsg::AgentPick { node, agent } => {
            app.dismiss_popup();
            set_card_config(app, &node, "agentId", agent.map(Value::String));
            Task::none()
        }
        CanvasChatMsg::ModelPick { node, pick } => {
            // 供应商与模型成对写入；跟随默认模型 = 两键一并移除
            app.dismiss_popup();
            set_card_config(
                app,
                &node,
                "providerId",
                pick.as_ref().map(|(p, _)| Value::String(p.clone())),
            );
            set_card_config(app, &node, "model", pick.map(|(_, m)| Value::String(m)));
            Task::none()
        }
        CanvasChatMsg::EffortPick { node, effort } => {
            app.dismiss_popup();
            set_card_config(app, &node, "reasoningEffort", effort.map(Value::String));
            Task::none()
        }
        CanvasChatMsg::Compact(node) => compact_card(app, &node),
        CanvasChatMsg::ScrollBottom(node) => {
            if let Some(chat) = app.canvas.as_mut().and_then(|s| s.view.card_chat.get_mut(&node)) {
                chat.follow_bottom = true;
            }
            scroll_bottom(&node)
        }
        CanvasChatMsg::PromptReady { gen, node, result } => {
            let Some(plan) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(&node))
                .and_then(|c| c.pending_send.take())
            else {
                return Task::none();
            };
            if gen != app.epoch() {
                return Task::none();
            }
            match result {
                Ok(prompt) => send_now(app, plan, Some(prompt)),
                Err(e) => {
                    restore_inputs(app, &node, plan.origin, plan.images, plan.refs);
                    set_error(app, &node, format!("Agent 提示词读取失败，已取消本次发送：{e}"));
                    Task::none()
                }
            }
        }
        CanvasChatMsg::RegenPromptReady { gen, node, result } => {
            let Some(history) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(&node))
                .and_then(|c| c.pending_regen.take())
            else {
                return Task::none();
            };
            if gen != app.epoch() {
                return Task::none();
            }
            match result {
                Ok(prompt) => {
                    return flow::regenerate(
                        app,
                        &CardStore::new(&node),
                        Some(prompt),
                        Some(history),
                    )
                }
                Err(e) => set_error(app, &node, format!("Agent 提示词读取失败，已取消重生成：{e}")),
            }
            Task::none()
        }
        CanvasChatMsg::Ready { gen, plan, result } => {
            if gen != app.epoch() {
                return Task::none();
            }
            match result {
                Ok(attachments) => finish_send(app, *plan, attachments),
                Err((images, e)) => {
                    restore_inputs(app, &plan.node, plan.origin, images, plan.refs.clone());
                    app.notify(crate::NoticeLevel::Warning, format!("图片发送失败：{e}"));
                    Task::none()
                }
            }
        }
    }
}

fn on_input(app: &mut App, node_id: &str, event: crate::chat_input::InputEvent) -> Task<Message> {
    use crate::chat_input::InputEvent;
    let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(node_id))
    else {
        return Task::none();
    };
    match event {
        InputEvent::Focus => {
            chat.draft.focused = true;
            chat.press_inside = true;
            // 面板与其它卡让出焦点（同一时刻只有一个输入面收键盘）
            app.chat.input.focused = false;
            if let Some(session) = app.canvas.as_mut() {
                for (id, other) in session.view.card_chat.iter_mut() {
                    if id != node_id {
                        other.draft.focused = false;
                    }
                }
            }
        }
        InputEvent::Blur => chat.draft.focused = false,
        InputEvent::Edited { text, caret, anchor } => {
            chat.draft.apply_edited(text, caret, anchor);
            // 首次真实输入占锁（幂等，已有锁不刷新 since）：只读身份与单机不占
            if crate::space_actions::canvas_lockable(app) {
                if let Some(s) = app.canvas.as_mut() {
                    s.acquire_lock(node_id, crate::now_millis());
                }
            }
        }
        InputEvent::Select { caret, anchor } => {
            chat.draft.caret = caret;
            chat.draft.anchor = anchor;
        }
        InputEvent::Preedit(preedit) => chat.draft.preedit = text_preedit::normalize(preedit),
        InputEvent::Undo => chat.draft.undo_step(),
        InputEvent::Redo => chat.draft.redo_step(),
        InputEvent::Send => {
            let node = node_id.to_string();
            // 发送任务必须交回 Elm 运行时：丢弃会既不发送，也不归还已取走的附件与引用
            return send(app, &node).chain(sync_mention_picker(app, &node));
        }
        InputEvent::PickerNav(delta) => {
            if let Some(p) = &mut chat.mention_picker {
                let len = p.candidates.len();
                if len > 0 {
                    let next = p.selected as i32 + delta;
                    p.selected = next.rem_euclid(len as i32) as usize;
                }
            }
            return card_mention_follow(app, node_id);
        }
        InputEvent::PickerCommit => {
            let selected = chat.mention_picker.as_ref().map_or(0, |p| p.selected);
            let node = node_id.to_string();
            commit_mention(app, &node, selected);
        }
        InputEvent::PickerClose => chat.mention_picker = None,
    }
    let node = node_id.to_string();
    sync_mention_picker(app, &node)
}

// ===== # 引用候选 =====

fn sync_mention_picker(app: &mut App, node_id: &str) -> Task<Message> {
    // 先只读取出触发段（候选收集要读整张画布与文件树）
    let Some(sync) = app
        .canvas
        .as_ref()
        .and_then(|s| s.view.card_chat.get(node_id))
        .and_then(|chat| flow::MentionSync::of(&chat.draft, chat.mention_picker.as_ref()))
    else {
        if let Some(chat) = app
            .canvas
            .as_mut()
            .and_then(|s| s.view.card_chat.get_mut(node_id))
        {
            chat.mention_picker = None;
        }
        return Task::none();
    };
    let candidates = card_candidates(app, node_id, sync.query());
    if let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(node_id))
    {
        sync.apply(&mut chat.mention_picker, candidates);
    }
    card_mention_follow(app, node_id)
}

/// 卡内候选浮层的跟滚任务（滚动区随卡缩放；浮层未开启时无事可做）。
fn card_mention_follow(app: &App, node_id: &str) -> Task<Message> {
    let Some(session) = app.canvas.as_ref() else {
        return Task::none();
    };
    let Some(picker) = session
        .view
        .card_chat
        .get(node_id)
        .and_then(|chat| chat.mention_picker.as_ref())
    else {
        return Task::none();
    };
    crate::chat_composer::mention_follow(
        &crate::chat_composer::ComposerScope::Card { node: node_id.to_string() },
        picker,
        session.view.zoom,
    )
}

/// 候选：画布内可引用卡优先（文本 / 媒体 / 表格），其后仓库文件与目录。
pub(crate) fn card_candidates(app: &App, node_id: &str, query: &str) -> Vec<MentionTarget> {
    let mut out = Vec::new();
    let lower = query.to_lowercase();
    if let Some(session) = &app.canvas {
        for n in &session.data.nodes {
            if n.id == node_id || n.node_type == "conversation" || n.node_type == "group" {
                continue;
            }
            let title = card_ref_title(n);
            if lower.is_empty() || title.to_lowercase().contains(&lower) {
                out.push(MentionTarget::Card { node_id: n.id.clone() });
            }
        }
    }
    out.extend(chat_actions::mention_candidates(app, query));
    out.truncate(50);
    out
}

/// 卡在候选里的显示标题（文件引用取文件名）。
fn card_ref_title(n: &atelyx_core::vault::CanvasFileNode) -> String {
    if n.node_type == "text" {
        if let Some(f) = n.data.get("file").and_then(Value::as_str) {
            return file_title(f);
        }
    }
    if let Some(title) = n.data.get("title").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        return title.to_string();
    }
    if let Some(name) = n.data.get("name").and_then(Value::as_str) {
        return name.to_string();
    }
    match n.data.get("file").and_then(Value::as_str) {
        Some(f) => file_title(f),
        None => n.node_type.clone(),
    }
}

/// 候选落地：卡片引用登记 + 建数据流边；文件引用沿用路径标签。触发段替换为标签文本。
fn commit_mention(app: &mut App, node_id: &str, index: usize) {
    let Some(target) = app
        .canvas
        .as_ref()
        .and_then(|s| s.view.card_chat.get(node_id))
        .and_then(|c| c.mention_picker.as_ref())
        .and_then(|p| p.candidates.get(index).cloned())
    else {
        return;
    };
    match target {
        MentionTarget::Card { node_id: source } => {
            append_card_ref(app, node_id, &source);
        }
        MentionTarget::Vault { rel, is_dir } => {
            let Some(chat) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(node_id))
            else {
                return;
            };
            let at = chat.mention_picker.as_ref().map(|p| p.at);
            let inserted = flow::commit_vault_ref(
                &mut chat.draft,
                &mut chat.mention_refs,
                rel,
                is_dir,
                at,
            );
            if inserted {
                chat.mention_picker = None;
            }
        }
    }
}

/// 来源卡引用的路径身份与引用种类（笔记卡 = 路径 + Note；其余 = Card，
/// 画布内文本卡以节点 id 作路径身份）。
fn card_ref_identity(target_node: &atelyx_core::vault::CanvasFileNode) -> (String, ai_refs::RefKind) {
    let file = target_node.data.get("file").and_then(Value::as_str);
    match (target_node.node_type.as_str(), file) {
        // 笔记引用：标签用「笔记」，注入走路径块；nodeId 仍登记用于消费判定
        ("text", Some(f)) => (f.to_string(), ai_refs::RefKind::Note),
        (_, Some(f)) => (f.to_string(), ai_refs::RefKind::Card),
        (_, None) => (target_node.id.clone(), ai_refs::RefKind::Card),
    }
}

/// 把来源卡引用插入对话卡草稿，并确保「来源 → 对话」数据流边（未消费虚线）。
pub fn append_card_ref(app: &mut App, conversation: &str, source: &str) {
    let Some(target_node) = node(app, source).cloned() else { return };
    let title = card_ref_title(&target_node);
    let (path, kind) = card_ref_identity(&target_node);
    let Some(session) = app.canvas.as_mut() else { return };
    let chat = session
        .view
        .card_chat
        .entry(conversation.to_string())
        .or_default();
    if chat.mention_refs.iter().any(|r| r.node_id.as_deref() == Some(source)) {
        chat.mention_picker = None;
        return;
    }
    let taken: Vec<(&str, &str)> = chat
        .mention_refs
        .iter()
        .map(|r| (r.path.as_str(), r.label.as_str()))
        .collect();
    let label = unique_card_label(&title, &taken);
    let tag = atelyx_core::ai::chat_send::mention_tag(kind, &label);
    let at = chat
        .mention_picker
        .as_ref()
        .map_or(chat.draft.caret, |p| p.at);
    insert_tag(&mut chat.draft, at, &tag);
    chat.mention_refs.push(ai_refs::RefEntry {
        path,
        label,
        kind,
        node_id: Some(source.to_string()),
    });
    chat.mention_picker = None;
    ensure_edge(app, source, conversation);
}

/// 面板绑定卡会话的候选落地：把来源卡引用登记进面板草稿并建数据流边（与卡内同语义）。
pub fn append_card_ref_to_panel(app: &mut App, source: &str) {
    let Some(target_node) = node(app, source).cloned() else { return };
    let Some(conversation) = app
        .chat
        .active
        .as_deref()
        .and_then(node_of_session)
        .map(str::to_string)
    else {
        return;
    };
    let (path, kind) = card_ref_identity(&target_node);
    let title = card_ref_title(&target_node);
    if app.chat.mention_refs.iter().any(|r| r.node_id.as_deref() == Some(source)) {
        app.chat.mention_picker = None;
        return;
    }
    let taken: Vec<(&str, &str)> = app
        .chat
        .mention_refs
        .iter()
        .map(|r| (r.path.as_str(), r.label.as_str()))
        .collect();
    let label = unique_card_label(&title, &taken);
    let tag = atelyx_core::ai::chat_send::mention_tag(kind, &label);
    let at = app
        .chat
        .mention_picker
        .as_ref()
        .map_or(app.chat.input.caret, |p| p.at);
    insert_tag(&mut app.chat.input, at, &tag);
    app.chat.mention_refs.push(ai_refs::RefEntry {
        path,
        label,
        kind,
        node_id: Some(source.to_string()),
    });
    app.chat.mention_picker = None;
    ensure_edge(app, source, &conversation);
}

fn unique_card_label(title: &str, taken: &[(&str, &str)]) -> String {
    atelyx_core::path::unique_name(title, |candidate| {
        taken.iter().any(|(_, label)| *label == candidate)
    })
}

fn insert_tag(draft: &mut crate::chat_input::Draft, at: usize, tag: &str) {
    let (text, caret) = chat_send::insert_mention(&draft.text, draft.caret, at, tag);
    draft.apply_edited(text, caret, caret);
}

fn trigger_mention(app: &mut App, node_id: &str) -> Task<Message> {
    let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(node_id))
    else {
        return Task::none();
    };
    chat.draft.focused = true;
    app.chat.input.focused = false;
    let (a, b) = chat.draft.selection();
    let mut text = String::with_capacity(chat.draft.text.len() - (b - a) + 1);
    text.push_str(&chat.draft.text[..a]);
    text.push('#');
    text.push_str(&chat.draft.text[b..]);
    chat.draft.apply_edited(text, a + 1, a + 1);
    sync_mention_picker(app, node_id)
}

/// 确保一条数据流边（同一源/目标只留一条，手绘关联边不参与去重）。
fn ensure_edge(app: &mut App, source: &str, target: &str) {
    let Some(session) = app.canvas.as_mut() else { return };
    if session
        .data
        .edges
        .iter()
        .any(|e| e.source == source && e.target == target && e.directed != Some(false))
    {
        return;
    }
    session.data.edges.push(edge(source, target));
    session.touch();
}

// ===== 附件 =====

fn pick_files(_app: &mut App, node_id: String) -> Task<Message> {
    Task::future(async move {
        let files = rfd::AsyncFileDialog::new()
            .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif"])
            .pick_files()
            .await;
        Message::Canvas(CanvasMsg::Chat(CanvasChatMsg::FilesPicked {
            node: node_id,
            paths: files
                .map(|fs| fs.into_iter().map(|f| f.path().to_path_buf()).collect())
                .unwrap_or_default(),
        }))
    })
}

/// 当前悬停的对话卡（外部文件拖入按落点卡路由；不看选中，防拖到别处误入托盘）。
pub fn hovered_conversation(app: &App) -> Option<String> {
    let session = app.canvas.as_ref()?;
    let candidate = session.view.hover.as_deref()?;
    session
        .data
        .nodes
        .iter()
        .find(|n| n.id == candidate && n.node_type == "conversation")
        .map(|n| n.id.clone())
}

pub(crate) fn add_attachments(app: &mut App, node_id: &str, paths: Vec<std::path::PathBuf>) {
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let Some(ext) = atelyx_core::vault::image_ext_of(&name) else {
            app.notify(
                crate::NoticeLevel::Warning,
                format!("暂不支持发送该类型文件：{name}（仅图片）"),
            );
            continue;
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                if let Some(chat) = app
                    .canvas
                    .as_mut()
                    .and_then(|s| s.view.card_chat.get_mut(node_id))
                {
                    chat.pending_images.push(PendingImage {
                        file_name: name,
                        bytes,
                        mime: chat_actions::image_mime(ext),
                    });
                }
            }
            Err(e) => app.notify(
                crate::NoticeLevel::Warning,
                format!("图片读取失败：{}（{e}）", path.display()),
            ),
        }
    }
}

// ===== 发送 =====

fn set_error(app: &mut App, node_id: &str, error: String) {
    // 面板绑定该卡会话时同屏展示同一错误：卡错误槽只在画布浮层可见
    let bound = app.chat.active.as_deref() == Some(session_id(node_id).as_str());
    if let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(node_id))
    {
        chat.error = Some(error.clone());
    }
    if bound {
        app.chat.error = Some(error);
    }
}

/// 清卡错误；面板绑定同一会话时同步清面板错误槽（与 [`set_error`] 的双写对称）。
fn clear_error(app: &mut App, node_id: &str) {
    let bound = app.chat.active.as_deref() == Some(session_id(node_id).as_str());
    if let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(node_id))
    {
        chat.error = None;
    }
    if bound {
        app.chat.error = None;
    }
}

/// 卡输入面发送入口：取草稿 → 解析模型 → 空间提示词预取 → 附件落盘 → 启动 run。
fn send(app: &mut App, node_id: &str) -> Task<Message> {
    let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(node_id))
    else {
        return Task::none();
    };
    let display = chat.draft.text.trim().to_string();
    let images = std::mem::take(&mut chat.pending_images);
    let refs = std::mem::take(&mut chat.mention_refs);
    send_with(app, node_id, display, refs, images, SendOrigin::Card)
}

/// 面板同会话发送入口（消息落到绑定的画布卡，不写面板会话真源）。
pub fn send_from_panel(app: &mut App, node_id: &str) -> Task<Message> {
    let display = app.chat.input.text.trim().to_string();
    let images = std::mem::take(&mut app.chat.pending_images);
    let refs = std::mem::take(&mut app.chat.mention_refs);
    send_with(app, node_id, display, refs, images, SendOrigin::Panel)
}

fn send_with(
    app: &mut App,
    node_id: &str,
    display: String,
    refs: Vec<ai_refs::RefEntry>,
    images: Vec<PendingImage>,
    origin: SendOrigin,
) -> Task<Message> {
    let Some(target) = node(app, node_id) else {
        restore_inputs(app, node_id, origin, images, refs);
        return Task::none();
    };
    if target.node_type != "conversation" || is_streaming(app, node_id) {
        restore_inputs(app, node_id, origin, images, refs);
        // 卡内输入区在生成中会换成提示条，面板没有；面板点发送要有反馈
        if origin == SendOrigin::Panel {
            app.notify(crate::NoticeLevel::Warning, "该会话已有回复进行中，消息未发送");
        }
        return Task::none();
    }
    if locked_by_peer(app, node_id) {
        restore_inputs(app, node_id, origin, images, refs);
        if origin == SendOrigin::Panel {
            app.notify(crate::NoticeLevel::Warning, "对端正在编辑该对话卡，消息未发送");
        }
        return Task::none();
    }
    if matches!(&app.repo_kind, RepoKind::Space(session) if !session.role.can_write()) {
        restore_inputs(app, node_id, origin, images, refs);
        app.notify(crate::NoticeLevel::Warning, "只读身份，无法发送");
        return Task::none();
    }
    if display.is_empty() && images.is_empty() {
        restore_inputs(app, node_id, origin, images, refs);
        return Task::none();
    }
    let (agent_id, model_pick, effort) = card_run_pick(app, node_id).expect("已确认卡存在");
    let plan = SendPlan {
        node: node_id.to_string(),
        display,
        refs,
        images,
        agent_id,
        model_pick,
        effort,
        origin,
        config: None,
    };
    // 空间模式 Agent 提示词文件在服务端：先取回再续跑
    if let Some(task) = prefetch_prompt(app, plan.clone()) {
        return task;
    }
    send_now(app, plan, None)
}

/// 早退归还：输入面原来的托盘与引用不得丢（半发状态不可接受）。
fn restore_inputs(
    app: &mut App,
    node_id: &str,
    origin: SendOrigin,
    images: Vec<PendingImage>,
    refs: Vec<ai_refs::RefEntry>,
) {
    match origin {
        SendOrigin::Card => {
            if let Some(chat) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(node_id))
            {
                chat.pending_images = images;
                chat.mention_refs = refs;
            }
        }
        SendOrigin::Panel => {
            app.chat.pending_images = images;
            app.chat.mention_refs = refs;
        }
    }
}

/// 卡级模型 / Agent / 推理等级：节点指定优先，其次仓库默认模型。
fn card_run_pick(
    app: &App,
    node_id: &str,
) -> Option<(Option<String>, Option<(String, String)>, Option<ReasoningEffort>)> {
    let node = node(app, node_id)?;
    let agent_id = node.data.get("agentId").and_then(Value::as_str).map(str::to_string);
    let card_provider = node
        .data
        .get("providerId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let card_model = node
        .data
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let model_pick = match (card_provider, card_model) {
        (Some(p), Some(m)) => Some((p.to_string(), m.to_string())),
        _ => match (&app.repo_config.model, &app.repo_config.model_provider_id) {
            (Some(m), Some(p)) => Some((p.clone(), m.clone())),
            _ => None,
        },
    };
    let effort = node
        .data
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .and_then(ReasoningEffort::parse_wire);
    Some((agent_id, model_pick, effort))
}

fn prefetch_prompt(app: &mut App, plan: SendPlan) -> Option<Task<Message>> {
    let RepoKind::Space(session) = &app.repo_kind else {
        return None;
    };
    let file = plan
        .agent_id
        .as_deref()
        .and_then(|id| app.agents.iter().find(|a| a.id == id))
        .and_then(|a| a.system_prompt_file.clone())?;
    let gen = app.epoch();
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    let node = plan.node.clone();
    if let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(&node))
    {
        chat.pending_send = Some(plan);
    }
    Some(Task::perform(
        async move {
            client
                .get_file_text(&space_id, &file)
                .await
                .map_err(|e| e.to_string())
        },
        move |result| {
            Message::Canvas(CanvasMsg::Chat(CanvasChatMsg::PromptReady { gen, node, result }))
        },
    ))
}

fn send_now(app: &mut App, plan: SendPlan, space_prompt: Option<String>) -> Task<Message> {
    if is_streaming(app, &plan.node) {
        return Task::none();
    }
    // 配置先行解析：失败即早退并归还托盘与引用（不产生半发消息）
    let mut plan = plan;
    match config_for(app, &plan, space_prompt) {
        Ok(c) => plan.config = Some(c),
        Err(e) => {
            restore_inputs(app, &plan.node, plan.origin, plan.images.clone(), plan.refs.clone());
            set_error(app, &plan.node, e);
            return Task::none();
        }
    }
    // 无图片：不经写盘后端，直接落消息启动 run（纯文本对话不需要空间内容写权限）
    if plan.images.is_empty() {
        return finish_send(app, plan, Vec::new());
    }
    let backend = match chat_actions::image_backend(app) {
        Ok(b) => b,
        Err(e) => {
            restore_inputs(app, &plan.node, plan.origin, plan.images.clone(), plan.refs.clone());
            set_error(app, &plan.node, e);
            return Task::none();
        }
    };
    let images = plan.images.clone();
    let plan_for_task = plan.clone();
    let sid = plan.media_session_id();
    let gen = app.epoch();
    // 图片落临时区（与面板同链；目录身份按卡会话 id 派生）
    Task::perform(
        chat_actions::materialize_images(backend, sid, images),
        move |result| {
            Message::Canvas(CanvasMsg::Chat(CanvasChatMsg::Ready {
                gen,
                plan: Box::new(plan_for_task.clone()),
                result,
            }))
        },
    )
}

/// 附件就绪：固化 user 消息、建引用边、启动直写 run。
fn finish_send(app: &mut App, plan: SendPlan, attachments: Vec<Value>) -> Task<Message> {
    let SendPlan { node: node_id, display, refs, images, origin, config, .. } = plan;
    let Some(config) = config else {
        set_error(app, &node_id, "发送配置缺失".to_string());
        restore_inputs(app, &node_id, origin, images, refs);
        return Task::none();
    };
    flow::finish_send(
        app,
        &CardStore::new(&node_id),
        SendReady { config, display, refs, images, attachments, origin },
    )
}

fn config_for(app: &App, plan: &SendPlan, space_prompt: Option<String>) -> Result<RunConfig, String> {
    let pick = plan
        .model_pick
        .as_ref()
        .map(|(p, m)| (p.as_str(), m.as_str()));
    let mut config = chat_actions::resolve_run_config(
        app,
        plan.agent_id.as_deref(),
        space_prompt,
        true,
        pick,
    )?;
    config.reasoning_effort = plan.effort;
    Ok(config)
}

fn start_run(app: &App, node_id: &str, config: RunConfig, history: Vec<Value>) -> Task<Message> {
    let Some(node) = node(app, node_id) else { return Task::none() };
    let compaction = node.data.get("compaction").cloned().filter(|c| !c.is_null());
    let ctx = chat_actions::tool_ctx(app, &config.agent);
    let hub = std::sync::Arc::clone(chat_actions::hub());
    let events = chat_actions::run_event_channel().0.clone();
    let sid = session_id(node_id);
    Task::future(async move {
        if let Err(e) = hub
            .start(RunPersist::Direct, events, sid.clone(), compaction, config, history, ctx)
            .await
        {
            let _ = chat_actions::run_event_channel().0.send(RunEvent::Failed {
                session_id: sid,
                message_id: String::new(),
                error: e,
            });
        }
        Message::Noop
    })
}

/// 引用命中 → 卡类型注入内容（图片附件路径单独返回）。
fn card_injection(app: &App, hits: &[ai_refs::RefHit], conversation: &str) -> (String, Vec<String>) {
    let Some(session) = &app.canvas else { return (String::new(), Vec::new()) };
    let mut contents = Vec::new();
    for hit in hits {
        // 笔记卡引用走路径块（模型按需读取），只有 Card 类型在此注入正文
        if hit.kind != ai_refs::RefKind::Card {
            continue;
        }
        let Some(source_id) = hit.node_id.as_deref() else { continue };
        if source_id == conversation {
            continue;
        }
        let Some(source) = session.data.nodes.iter().find(|n| n.id == source_id) else {
            continue;
        };
        let title = card_ref_title(source);
        let source_content = match source.node_type.as_str() {
            "text" => {
                if let Some(file) = source.data.get("file").and_then(Value::as_str) {
                    match session.view.refs.get(file) {
                        Some(RefContent::Text(t)) => {
                            CardSource::MediaText { title, body: atelyx_core::frontmatter::parse(t).body }
                        }
                        Some(RefContent::Missing) => CardSource::Missing { title },
                        _ => CardSource::Missing { title },
                    }
                } else {
                    let body = source.data.get("bodyMd").and_then(Value::as_str).unwrap_or("");
                    CardSource::InlineText { title, body: body.to_string() }
                }
            }
            "table" => match source
                .data
                .get("file")
                .and_then(Value::as_str)
                .and_then(|f| session.view.refs.get(f))
            {
                Some(RefContent::Table { body, .. }) => {
                    CardSource::TableText { title, body: body.clone() }
                }
                _ => CardSource::Missing { title },
            },
            "media" => {
                let kind = source.data.get("kind").and_then(Value::as_str).unwrap_or("file");
                let file = source.data.get("file").and_then(Value::as_str);
                match (kind, file) {
                    ("image", Some(f)) => match session.view.refs.get(f) {
                        Some(RefContent::Missing) => CardSource::Missing { title },
                        _ => CardSource::Image { title, file: f.to_string() },
                    },
                    (_, Some(f)) => match session.view.refs.get(f) {
                        Some(RefContent::Text(body)) => {
                            CardSource::MediaText { title, body: body.clone() }
                        }
                        Some(RefContent::Binary) => CardSource::Binary { title },
                        _ => CardSource::Missing { title },
                    },
                    _ => CardSource::Missing { title },
                }
            }
            _ => continue,
        };
        contents.push(CardContent { node_id: source_id.to_string(), source: source_content });
    }
    canvas_refs::card_injection(hits, &contents)
}

fn regenerate(app: &mut App, node_id: &str) -> Task<Message> {
    flow::regenerate(app, &CardStore::new(node_id), None, None)
}

/// 空间模式 Agent 提示词文件在服务端：重生成前异步取一次（回执带待跑历史续跑）。
fn prefetch_regen_prompt(app: &mut App, node_id: &str, history: Vec<Value>) -> Option<Task<Message>> {
    let RepoKind::Space(session) = &app.repo_kind else {
        return None;
    };
    let agent_id = node(app, node_id)
        .and_then(|n| n.data.get("agentId").and_then(Value::as_str))
        .map(str::to_string);
    let file = agent_id
        .as_deref()
        .and_then(|id| app.agents.iter().find(|a| a.id == id))
        .and_then(|a| a.system_prompt_file.clone())?;
    let gen = app.epoch();
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    let node_id = node_id.to_string();
    if let Some(chat) = app
        .canvas
        .as_mut()
        .and_then(|s| s.view.card_chat.get_mut(&node_id))
    {
        chat.pending_regen = Some(history);
    }
    Some(Task::perform(
        async move {
            client
                .get_file_text(&space_id, &file)
                .await
                .map_err(|e| e.to_string())
        },
        move |result| {
            Message::Canvas(CanvasMsg::Chat(CanvasChatMsg::RegenPromptReady {
                gen,
                node: node_id,
                result,
            }))
        },
    ))
}

fn rollback(app: &mut App, node_id: &str, message_id: &str) {
    let _ = flow::rollback(app, &CardStore::new(node_id), message_id);
}

fn branch(app: &mut App, node_id: &str, message_id: &str) -> Task<Message> {
    if locked_by_peer(app, node_id) {
        return Task::none();
    }
    let Some(session) = app.canvas.as_mut() else { return Task::none() };
    let Some(messages) = messages_of_session(session, node_id) else { return Task::none() };
    let Some(idx) = messages
        .iter()
        .position(|m| m.get("id").and_then(Value::as_str) == Some(message_id))
    else {
        return Task::none();
    };
    let kept = messages[..=idx].to_vec();
    let Some(source) = session.data.nodes.iter().find(|n| n.id == node_id).cloned() else {
        return Task::none();
    };
    let (w, _h) = crate::canvas_view::node_size(&source);
    let start = (source.x + w + CARD_GAP, source.y);
    let spot = crate::canvas_view::find_free_spot(&session.data.nodes, start, (520.0, 420.0), SPOT_STEP);
    let mut data = source.data.clone();
    data["messages"] = Value::Array(kept);
    let id = chat_actions::uuid_v4();
    session.data.nodes.push(atelyx_core::vault::CanvasFileNode {
        id: id.clone(),
        node_type: "conversation".into(),
        x: spot.0,
        y: spot.1,
        width: Some(520.0),
        height: Some(420.0),
        data,
    });
    session.data.edges.push(edge(node_id, &id));
    session.touch();
    session
        .view
        .card_chat
        .entry(id)
        .or_default()
        .follow_bottom = true;
    Task::none()
}

// ===== 卡配置与压缩 =====

/// 写卡节点配置（Agent / 模型 / 推理等级）：value None = 移除键回落默认。
/// 被对端独占编辑的卡不写（配置属卡内容，锁作用范围内不落盘）。
fn set_card_config(app: &mut App, node_id: &str, key: &str, value: Option<Value>) {
    if locked_by_peer(app, node_id) {
        return;
    }
    let Some(session) = app.canvas.as_mut() else { return };
    let Some(n) = session
        .data
        .nodes
        .iter_mut()
        .find(|n| n.id == node_id && n.node_type == "conversation")
    else {
        return;
    };
    let Some(obj) = n.data.as_object_mut() else { return };
    let next = value.unwrap_or(Value::Null);
    if obj.get(key) == Some(&next) {
        return;
    }
    if next.is_null() {
        obj.remove(key);
    } else {
        obj.insert(key.into(), next);
    }
    session.touch();
}

/// 卡会话压缩：全部消息折进检查点（≥2 条），摘要写节点压缩注解（非破坏）。
fn compact_card(app: &mut App, node_id: &str) -> Task<Message> {
    flow::compact(app, &CardStore::new(node_id))
}

/// 卡会话压缩回执：摘要写节点压缩注解（锚点 = 请求时刻末条），失败提示进卡。
pub(crate) fn compact_done(app: &mut App, node_id: &str, result: Option<String>) {
    flow::compact_done(app, &CardStore::new(node_id), result);
}

// ===== run 事件 =====

/// 应用一条直写 run 事件（画布卡会话）。
pub fn on_run_event(app: &mut App, event: RunEvent) -> Task<Message> {
    let (run_session, message_id) = match &event {
        RunEvent::Started { session_id, message_id }
        | RunEvent::TextDelta { session_id, message_id, .. }
        | RunEvent::ReasoningDelta { session_id, message_id, .. }
        | RunEvent::ToolStart { session_id, message_id, .. }
        | RunEvent::ToolEnd { session_id, message_id, .. }
        | RunEvent::Finished { session_id, message_id, .. }
        | RunEvent::Failed { session_id, message_id, .. }
        | RunEvent::Snapshot { session_id, message_id, .. }
        | RunEvent::MessageRemoved { session_id, message_id } => {
            (session_id.clone(), message_id.clone())
        }
    };
    let Some(node_id) = node_of_session(&run_session).map(str::to_string) else {
        return Task::none();
    };
    match event {
        RunEvent::Started { .. } => {
            if let Some(chat) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(&node_id))
            {
                chat.streaming = true;
                chat.error = None;
            }
            if app.chat.active.as_deref() == Some(session_id(&node_id).as_str()) {
                app.chat.error = None;
            }
        }
        RunEvent::Snapshot { message, .. } => {
            let Some(session) = app.canvas.as_mut() else { return Task::none() };
            let changed = upsert_message(session, &node_id, message);
            if changed {
                session.touch();
                refresh_card_docs(session);
            }
            let follow = session
                .view
                .card_chat
                .get(&node_id)
                .is_some_and(|c| c.follow_bottom);
            let panel_bound = app.chat.active.as_deref() == Some(session_id(&node_id).as_str());
            refresh_panel_if_bound(app, &node_id);
            if follow {
                let task = scroll_bottom(&node_id);
                // 面板同会话双开：两边都贴底
                return if panel_bound {
                    task.chain(chat_actions::scroll_flow_bottom())
                } else {
                    task
                };
            }
        }
        RunEvent::MessageRemoved { .. } => {
            let Some(session) = app.canvas.as_mut() else { return Task::none() };
            if remove_message(session, &node_id, &message_id) {
                session.touch();
                refresh_card_docs(session);
            }
            refresh_panel_if_bound(app, &node_id);
        }
        RunEvent::Finished { .. } => {
            let Some(session) = app.canvas.as_mut() else { return Task::none() };
            if let Some(chat) = session.view.card_chat.get_mut(&node_id) {
                chat.streaming = false;
                chat.follow_bottom = true;
            }
            refresh_card_docs(session);
            refresh_panel_if_bound(app, &node_id);
            if let Some(task) = materialize(app, &node_id, &message_id) {
                return task;
            }
        }
        RunEvent::Failed { error, .. } => {
            let Some(session) = app.canvas.as_mut() else { return Task::none() };
            if let Some(chat) = session.view.card_chat.get_mut(&node_id) {
                chat.streaming = false;
                if !error.is_empty() {
                    chat.error = Some(error);
                }
            }
            refresh_card_docs(session);
            refresh_panel_if_bound(app, &node_id);
        }
        _ => {
            // 流式拍由 Snapshot 携带完整消息；增量事件只维持流式态
        }
    }
    Task::none()
}

// ===== 产出落卡 =====

/// 落卡产物：文件引用卡（读 / 写）或最终正文文本卡。
enum Material {
    OutputText(String),
    OutputFile(String),
    InputFile(String),
}

fn materialize(app: &mut App, node_id: &str, message_id: &str) -> Option<Task<Message>> {
    if !app.repo_config.canvas_auto_materialize.unwrap_or(true) {
        return None;
    }
    let session = app.canvas.as_ref()?;
    let node = session.data.nodes.iter().find(|n| n.id == node_id)?;
    let message = node
        .data
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|ms| ms.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(message_id)))?;
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
            let Some(path) = run.get("args").and_then(|a| a.get("path")).and_then(Value::as_str) else {
                continue;
            };
            // 只有「单个文件」类工具落卡：grep / glob 的 path 是限定目录，落卡会得到破卡；
            // 删除的目标已不存在、移动的 path 参数是源路径，同样不落卡
            match name {
                "read_file" | "read_history" | "table_read" | "canvas_read" => {
                    materials.push(Material::InputFile(path.to_string()))
                }
                "write_file" | "append_file" | "edit_file" | "table_edit" | "canvas_edit" => {
                    materials.push(Material::OutputFile(path.to_string()))
                }
                _ => {}
            }
        }
    }
    if readable {
        materials.push(Material::OutputText(content.to_string()));
    }
    apply_materials(app, node_id, materials)
}

fn apply_materials(app: &mut App, conversation: &str, materials: Vec<Material>) -> Option<Task<Message>> {
    let session = app.canvas.as_mut()?;
    let conv = session.data.nodes.iter().find(|n| n.id == conversation).cloned()?;
    let (cw, _ch) = crate::canvas_view::node_size(&conv);
    let mut changed = false;
    let mut new_paths: Vec<String> = Vec::new();
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
                let spot = crate::canvas_view::find_free_spot(
                    &session.data.nodes,
                    (conv.x + cw + CARD_GAP, conv.y),
                    (TEXT_CARD_W, TEXT_CARD_H),
                    SPOT_STEP,
                );
                let id = chat_actions::uuid_v4();
                let title = first_line_prefix(&text, 16);
                session.data.nodes.push(atelyx_core::vault::CanvasFileNode {
                    id: id.clone(),
                    node_type: "text".into(),
                    x: spot.0,
                    y: spot.1,
                    width: Some(TEXT_CARD_W),
                    height: Some(TEXT_CARD_H),
                    data: json!({ "title": title, "bodyMd": text }),
                });
                session.view.text_docs.insert(
                    id.clone(),
                    crate::markdown_view::parse_document(&text),
                );
                session.data.edges.push(edge(conversation, &id));
                changed = true;
            }
            Material::OutputFile(path) => {
                if let Some(existing) = find_file_card(session, &path) {
                    if !has_edge(session, conversation, existing) {
                        session.data.edges.push(edge(conversation, existing));
                        changed = true;
                    }
                    continue;
                }
                let Some(card) = file_card(&path) else { continue };
                let size = (card.1, card.2);
                let spot = crate::canvas_view::find_free_spot(
                    &session.data.nodes,
                    (conv.x + cw + CARD_GAP, conv.y),
                    size,
                    SPOT_STEP,
                );
                let id = chat_actions::uuid_v4();
                session.data.nodes.push(atelyx_core::vault::CanvasFileNode {
                    id: id.clone(),
                    node_type: card.0.into(),
                    x: spot.0,
                    y: spot.1,
                    width: Some(card.1),
                    height: Some(card.2),
                    data: card.3,
                });
                session.data.edges.push(edge(conversation, &id));
                new_paths.push(path);
                changed = true;
            }
            Material::InputFile(path) => {
                if let Some(existing) = find_file_card(session, &path) {
                    if !has_edge(session, existing, conversation) {
                        session.data.edges.push(edge(existing, conversation));
                        changed = true;
                    }
                    continue;
                }
                let Some(card) = file_card(&path) else { continue };
                let size = (card.1, card.2);
                let spot = crate::canvas_view::find_free_spot(
                    &session.data.nodes,
                    (conv.x - size.0 - CARD_GAP, conv.y),
                    size,
                    SPOT_STEP,
                );
                let id = chat_actions::uuid_v4();
                session.data.nodes.push(atelyx_core::vault::CanvasFileNode {
                    id: id.clone(),
                    node_type: card.0.into(),
                    x: spot.0,
                    y: spot.1,
                    width: Some(card.1),
                    height: Some(card.2),
                    data: card.3,
                });
                session.data.edges.push(edge(&id, conversation));
                new_paths.push(path);
                changed = true;
            }
        }
    }
    if changed {
        session.touch();
        refresh_card_docs(session);
    }
    if new_paths.is_empty() {
        None
    } else {
        // 新引用卡的正文 / 图片按需载入（与画布打开同一读取口径）
        Some(crate::canvas_actions::load_refs(app, new_paths))
    }
}

fn edge(source: &str, target: &str) -> atelyx_core::vault::CanvasFileEdge {
    atelyx_core::vault::CanvasFileEdge {
        id: chat_actions::uuid_v4(),
        source: source.to_string(),
        target: target.to_string(),
        source_handle: None,
        target_handle: None,
        directed: None,
        link_mode: None,
        created_at: (chat_actions::now_ms() / 1000) as i64,
    }
}

fn find_file_card<'a>(session: &'a CanvasSession, path: &str) -> Option<&'a str> {
    session
        .data
        .nodes
        .iter()
        .find(|n| {
            n.data.get("file").and_then(Value::as_str) == Some(path)
                && matches!(n.node_type.as_str(), "text" | "table" | "media")
        })
        .map(|n| n.id.as_str())
}

fn has_edge(session: &CanvasSession, source: &str, target: &str) -> bool {
    session
        .data
        .edges
        .iter()
        .any(|e| e.source == source && e.target == target && e.directed != Some(false))
}

/// 文件 → 卡类型与默认尺寸（文本卡引用 / 表格薄卡 / 媒体卡）。
fn file_card(path: &str) -> Option<(&'static str, f64, f64, Value)> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let title = file_title(path);
    match ext.as_str() {
        "md" => Some(("text", TEXT_CARD_W, TEXT_CARD_H, json!({ "title": title, "file": path }))),
        "atb" => Some(("table", 300.0, 110.0, json!({ "title": title, "file": path }))),
        "atlx" => None,
        _ if atelyx_core::vault::image_ext_of(name).is_some() => Some((
            "media",
            260.0,
            240.0,
            json!({
                "file": path,
                "kind": "image",
                "mime": chat_actions::image_mime(&ext),
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

// ===== 存储视图（面板骨架的卡侧实现） =====

/// 画布卡会话的存储视图：消息真源在节点 data.messages，写入即节点脏。
struct CardStore {
    node: String,
}

impl CardStore {
    fn new(node: &str) -> Self {
        Self { node: node.to_string() }
    }
}

impl ChatStore for CardStore {
    fn session_id(&self) -> String {
        session_id(&self.node)
    }

    fn messages(&self, app: &App) -> Vec<Value> {
        messages_of(app, &self.session_id()).unwrap_or(&[]).to_vec()
    }

    fn history(&self, app: &App) -> Vec<Value> {
        self.messages(app)
    }

    fn append(&self, app: &mut App, message: Value) -> Result<(), String> {
        let Some(session) = app.canvas.as_mut() else {
            return Err("画布未打开".to_string());
        };
        if !upsert_message(session, &self.node, message) {
            return Err("对话卡不存在".to_string());
        }
        session.touch();
        refresh_card_docs(session);
        Ok(())
    }

    fn truncate_to(&self, app: &mut App, history: &mut Vec<Value>) -> Result<(), String> {
        let Some(session) = app.canvas.as_mut() else {
            return Err("画布未打开".to_string());
        };
        if messages_of_session(session, &self.node).is_none() {
            return Err("对话卡不存在".to_string());
        }
        replace_messages(session, &self.node, history);
        session.touch();
        refresh_card_docs(session);
        Ok(())
    }

    fn write_compaction(&self, app: &mut App, record: Value) -> Result<(), String> {
        let Some(session) = app.canvas.as_mut() else {
            return Err("画布未打开".to_string());
        };
        let Some(node) = session.data.nodes.iter_mut().find(|n| n.id == self.node) else {
            return Err("对话卡不存在".to_string());
        };
        if let Some(data) = node.data.as_object_mut() {
            data.insert("compaction".into(), record);
        }
        session.touch();
        refresh_card_docs(session);
        refresh_panel_if_bound(app, &self.node);
        Ok(())
    }

    fn compaction(&self, app: &App) -> Option<Value> {
        node(app, &self.node)
            .and_then(|n| n.data.get("compaction").cloned())
            .filter(|c| !c.is_null())
    }

    fn streaming(&self, app: &App) -> bool {
        is_streaming(app, &self.node)
    }

    fn blocked(&self, app: &App, action: ChatAction) -> bool {
        if matches!(&app.repo_kind, crate::RepoKind::Space(session) if !session.role.can_write()) {
            return true;
        }
        let streaming = is_streaming(app, &self.node);
        let locked = locked_by_peer(app, &self.node);
        let compacting = card_chat(app, &self.node).is_some_and(|c| c.compacting);
        match action {
            // 压缩在途同样禁回滚：摘要锚点会与截断后的消息集错位
            ChatAction::Regenerate | ChatAction::Rollback => streaming || locked || compacting,
            ChatAction::Compact => streaming || locked || compacting,
        }
    }

    fn blocked_notice(&self, app: &App, action: ChatAction) -> String {
        if matches!(&app.repo_kind, crate::RepoKind::Space(session) if !session.role.can_write()) {
            return "只读身份，无法操作".to_string();
        }
        if locked_by_peer(app, &self.node) {
            return "对端正在编辑该对话卡，暂不能操作".to_string();
        }
        if card_chat(app, &self.node).is_some_and(|c| c.compacting) {
            return "该会话正在压缩，暂不能操作".to_string();
        }
        if is_streaming(app, &self.node) {
            return match action {
                ChatAction::Regenerate => "该会话已有回复进行中，无法重新生成",
                ChatAction::Rollback => "该会话已有回复进行中，无法删除其后消息",
                ChatAction::Compact => "该会话已有回复进行中，无法压缩",
            }
            .to_string();
        }
        "当前状态不可执行该操作".to_string()
    }

    fn run_config(
        &self,
        app: &App,
        space_prompt: Option<String>,
        for_run: bool,
    ) -> Result<RunConfig, String> {
        let (agent_id, model_pick, effort) =
            card_run_pick(app, &self.node).ok_or_else(|| "对话卡不存在".to_string())?;
        let pick = model_pick.as_ref().map(|(p, m)| (p.as_str(), m.as_str()));
        let mut config =
            chat_actions::resolve_run_config(app, agent_id.as_deref(), space_prompt, for_run, pick)?;
        config.reasoning_effort = effort;
        Ok(config)
    }

    fn ref_shape(&self) -> RefsShape {
        RefsShape::Card
    }

    fn injection(&self, app: &App, hits: &[RefHit]) -> (String, Vec<String>) {
        card_injection(app, hits, &self.node)
    }

    fn link_refs(&self, app: &mut App, refs: &[RefEntry]) {
        for id in refs.iter().filter_map(|r| r.node_id.clone()) {
            ensure_edge(app, &id, &self.node);
        }
    }

    fn target_ready(&self, app: &App) -> bool {
        node(app, &self.node).is_some()
    }

    fn restore_inputs(
        &self,
        app: &mut App,
        images: Vec<PendingImage>,
        refs: Vec<RefEntry>,
        origin: SendOrigin,
    ) {
        restore_inputs(app, &self.node, origin, images, refs);
    }

    fn set_error(&self, app: &mut App, error: String) {
        set_error(app, &self.node, error);
    }

    fn append_failed(&self, app: &mut App, error: String) {
        app.notify(crate::NoticeLevel::Error, error);
    }

    fn busy_notice(&self) -> &'static str {
        "该会话已有回复进行中，消息未发送"
    }

    fn after_append(&self, app: &mut App, origin: SendOrigin) {
        let Some(session) = app.canvas.as_mut() else { return };
        if let Some(chat) = session.view.card_chat.get_mut(&self.node) {
            chat.error = None;
            chat.follow_bottom = true;
            chat.streaming = true;
            if origin == SendOrigin::Card {
                chat.draft.clear();
                chat.mention_picker = None;
            }
        }
        // 回执到达时用户可能已切到别的会话：只清绑定的那个会话的错误与草稿
        if origin == SendOrigin::Panel
            && app.chat.active.as_deref() == Some(session_id(&self.node).as_str())
        {
            app.chat.error = None;
            app.chat.input.clear();
            app.chat.mention_picker = None;
            app.chat.follow_bottom = true;
        }
        refresh_panel_if_bound(app, &self.node);
    }

    fn rebuild_user_content(&self, app: &App, message: &mut Value) {
        let refs = chat_actions::message_refs(message);
        let display = message
            .get("displayContent")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| message.get("content").and_then(Value::as_str).unwrap_or(""))
            .to_string();
        let hits = ai_refs::scan_hits(&display, &refs);
        let path_block = ai_refs::reference_path_block(&hits);
        // 重生成按当下卡内容重拼：会话身份缺省（自引用也照常注入）
        let (card_block, _) = card_injection(app, &hits, "");
        message["content"] = json!(chat_send::message_content(&path_block, &card_block, &display));
    }

    fn prefetch_regenerate(&self, app: &mut App, history: Vec<Value>) -> Option<Task<Message>> {
        prefetch_regen_prompt(app, &self.node, history)
    }

    fn refresh(&self, app: &mut App, action: ChatAction) {
        if action == ChatAction::Regenerate {
            if let Some(chat) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.card_chat.get_mut(&self.node))
            {
                chat.error = None;
                chat.streaming = true;
                chat.follow_bottom = true;
            }
            if app.chat.active.as_deref() == Some(session_id(&self.node).as_str()) {
                app.chat.error = None;
            }
        }
        refresh_panel_if_bound(app, &self.node);
    }

    fn start_run(&self, app: &App, config: RunConfig, history: Vec<Value>) -> Task<Message> {
        start_run(app, &self.node, config, history)
    }

    fn tail(&self, app: &App, image_files: &[String]) -> Task<Message> {
        let task = scroll_bottom(&self.node);
        if image_files.is_empty() {
            return task;
        }
        task.chain(crate::canvas_actions::load_refs(app, image_files.to_vec()))
    }

    fn compact_precheck(&self, _app: &App, messages: &[Value]) -> Result<(), String> {
        match atelyx_core::ai::chat_send::compaction_anchor(messages) {
            Some(_) => Ok(()),
            None => Err("压缩失败：会话记录不完整，无法确定截止消息".to_string()),
        }
    }

    fn mark_compacting(&self, app: &mut App, messages: &[Value]) {
        let anchor = atelyx_core::ai::chat_send::compaction_anchor(messages);
        if let Some(chat) = app
            .canvas
            .as_mut()
            .and_then(|s| s.view.card_chat.get_mut(&self.node))
        {
            chat.compacting = true;
            chat.compact_anchor = anchor;
        }
    }

    fn clear_compacting(&self, app: &mut App) -> Option<(String, usize)> {
        // 压缩期间被拒留下的提示已过期（与面板口径一致）
        if let Some(chat) = app
            .canvas
            .as_mut()
            .and_then(|s| s.view.card_chat.get_mut(&self.node))
        {
            if chat.error.as_deref().is_some_and(|e| e.contains("正在压缩")) {
                chat.error = None;
            }
        }
        if app.chat.active.as_deref() == Some(session_id(&self.node).as_str())
            && app.chat.error.as_deref().is_some_and(|e| e.contains("正在压缩"))
        {
            app.chat.error = None;
        }
        app.canvas
            .as_mut()
            .and_then(|s| s.view.card_chat.get_mut(&self.node))
            .and_then(|chat| {
                chat.compacting = false;
                chat.compact_anchor.take()
            })
    }

    fn missing_anchor_error(&self) -> Option<&'static str> {
        Some("压缩失败：会话记录不完整，无法确定截止消息")
    }
}

// ===== 面板同会话 =====

/// 面板绑定会话的重新生成（委托卡内实现）。
pub fn regenerate_card(app: &mut App, node_id: &str) -> Task<Message> {
    regenerate(app, node_id)
}

/// 面板绑定会话的删除其后消息（委托卡内实现）。
pub fn rollback_card(app: &mut App, node_id: &str, message_id: &str) {
    rollback(app, node_id, message_id);
}

/// 在 AI 面板打开该卡会话（同一份消息真源的双视图）。
pub fn open_in_panel(app: &mut App, node_id: &str) {
    let Some(node) = node(app, node_id) else { return };
    if node.node_type != "conversation" {
        return;
    }
    if !crate::layout_view::window_has_view(app, "aichat") {
        app.notify(crate::NoticeLevel::Warning, "布局中没有 AI 对话面板，请先在布局中添加");
        return;
    }
    let sid = session_id(node_id);
    app.chat.active = Some(sid);
    app.chat.selection = None;
    app.chat.rollback_confirm = None;
    app.chat.error = None;
    app.chat.follow_bottom = true;
    app.chat.sidebar_open = false;
    refresh_panel_docs_only(app, node_id);
}

/// 面板绑定画布会话时同步解析缓存（消息 / 标题由 `messages_of` 直接读卡）。
pub fn refresh_panel_if_bound(app: &mut App, node_id: &str) {
    let sid = session_id(node_id);
    if app.chat.active.as_deref() == Some(sid.as_str()) {
        refresh_panel_docs_only(app, node_id);
    }
}

fn refresh_panel_docs_only(app: &mut App, node_id: &str) {
    if let Some(session) = &app.canvas {
        if let Some(messages) = messages_of_session(session, node_id) {
            cache_docs(messages, &mut app.chat.docs);
        }
    }
}

// ===== 状态工具 =====

fn messages_of_session<'a>(session: &'a CanvasSession, node_id: &str) -> Option<&'a [Value]> {
    session
        .data
        .nodes
        .iter()
        .find(|n| n.id == node_id)?
        .data
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
}

fn upsert_message(session: &mut CanvasSession, node_id: &str, message: Value) -> bool {
    let Some(node) = session.data.nodes.iter_mut().find(|n| n.id == node_id) else {
        return false;
    };
    let Some(obj) = node.data.as_object_mut() else { return false };
    let messages = obj
        .entry("messages")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(arr) = messages.as_array_mut() else {
        *messages = Value::Array(Vec::new());
        let arr = messages.as_array_mut().expect("刚写入数组");
        arr.push(message);
        return true;
    };
    let id = message.get("id").and_then(Value::as_str).map(str::to_string);
    if let Some(id) = id {
        if let Some(slot) = arr
            .iter_mut()
            .find(|m| m.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            if *slot == message {
                return false;
            }
            *slot = message;
            return true;
        }
    }
    arr.push(message);
    true
}

fn remove_message(session: &mut CanvasSession, node_id: &str, message_id: &str) -> bool {
    let Some(node) = session.data.nodes.iter_mut().find(|n| n.id == node_id) else {
        return false;
    };
    let Some(arr) = node
        .data
        .get_mut("messages")
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    let before = arr.len();
    arr.retain(|m| m.get("id").and_then(Value::as_str) != Some(message_id));
    arr.len() != before
}

fn replace_messages(session: &mut CanvasSession, node_id: &str, messages: &[Value]) {
    if let Some(node) = session.data.nodes.iter_mut().find(|n| n.id == node_id) {
        if let Some(obj) = node.data.as_object_mut() {
            obj.insert("messages".into(), Value::Array(messages.to_vec()));
        }
    }
}

/// 画布消息正文的 markdown 解析缓存（多张对话卡共用一份；按全部卡的哈希收敛）。
pub(crate) fn refresh_card_docs(session: &mut CanvasSession) {
    let mut docs = std::mem::take(&mut session.view.docs);
    let mut used: HashSet<u64> = HashSet::new();
    for n in &session.data.nodes {
        if n.node_type != "conversation" {
            continue;
        }
        if let Some(messages) = n.data.get("messages").and_then(Value::as_array) {
            collect_docs(messages, &mut docs, &mut used);
        }
    }
    docs.retain(|k, _| used.contains(k));
    session.view.docs = docs;
}

fn cache_docs(messages: &[Value], docs: &mut HashMap<u64, atelyx_core::markdown::MarkdownDocument>) {
    let mut used: HashSet<u64> = HashSet::new();
    collect_docs(messages, docs, &mut used);
    docs.retain(|k, _| used.contains(k));
}

fn collect_docs(
    messages: &[Value],
    docs: &mut HashMap<u64, atelyx_core::markdown::MarkdownDocument>,
    used: &mut HashSet<u64>,
) {
    for m in messages {
        let mentions = chat_actions::message_mentions(m);
        let has_mentions = !mentions.is_empty();
        let sources = [
            m.get("displayContent").and_then(Value::as_str),
            m.get("content").and_then(Value::as_str),
        ];
        for src in sources.into_iter().flatten() {
            if src.is_empty() {
                continue;
            }
            let is_user_display = m["role"] == json!("user")
                && has_mentions
                && m.get("displayContent").and_then(Value::as_str) == Some(src);
            if is_user_display {
                let sig: Vec<String> = mentions.iter().map(|mn| mn.label.clone()).collect();
                let key = chat_actions::doc_key(src, &sig);
                used.insert(key);
                if !docs.contains_key(&key) {
                    docs.insert(
                        key,
                        crate::markdown_view::parse_document_with_mentions(src, mentions.clone()),
                    );
                }
            } else {
                let key = chat_actions::source_hash(src);
                used.insert(key);
                if !docs.contains_key(&key) {
                    docs.insert(key, crate::markdown_view::parse_document(src));
                }
            }
        }
        if let Some(steps) = m.get("steps").and_then(Value::as_array) {
            for st in steps {
                if st.get("kind").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = st.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            let key = chat_actions::source_hash(text);
                            used.insert(key);
                            docs.entry(key)
                                .or_insert_with(|| crate::markdown_view::parse_document(text));
                        }
                    }
                }
            }
        }
    }
}

/// 卡片消息流滚动容器 id（回底命令与视图挂载共用）。
pub fn flow_id(node_id: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("canvas-chat-{node_id}"))
}

fn scroll_bottom(node_id: &str) -> Task<Message> {
    iced::widget::operation::scroll_to(
        flow_id(node_id),
        iced::widget::scrollable::AbsoluteOffset { x: Some(0.0), y: Some(1.0e9) },
    )
}

#[cfg(test)]
mod tests {
    /// 槽位对齐：中间删卡留空槽（后续卡下标不变），末尾删卡回收，新卡追加。
    #[test]
    fn overlay_slots_keep_middle_indexes_stable() {
        let mut slots = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        sync_overlay_slots(&mut slots, &["a".to_string(), "c".to_string()]);
        assert_eq!(slots, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        sync_overlay_slots(&mut slots, &["a".to_string(), "c".to_string(), "d".to_string()]);
        assert_eq!(
            slots,
            vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()]
        );
        // 末尾连续删除才回收
        sync_overlay_slots(&mut slots, &["a".to_string(), "b".to_string()]);
        assert_eq!(slots, vec!["a".to_string(), "b".to_string()]);
    }

    use super::*;

    #[test]
    fn session_ids_are_scoped_to_canvas_cards() {
        assert_eq!(session_id("n1"), "canvas:n1");
        assert_eq!(node_of_session("canvas:n1"), Some("n1"));
        // 面板会话 id 不被误认
        assert_eq!(node_of_session("uuid-1234"), None);
    }

    #[test]
    fn file_cards_map_by_extension() {
        assert_eq!(file_card("笔记/方案.md").map(|c| c.0), Some("text"));
        assert_eq!(file_card("数据/表.atb").map(|c| c.0), Some("table"));
        assert_eq!(file_card("素材/图.png").map(|c| c.0), Some("media"));
        assert_eq!(file_card("素材/包.zip").map(|c| c.0), Some("media"));
        // 嵌套画布不从产出落卡
        assert!(file_card("项目/画布.atlx").is_none());
        let card = file_card("素材/图.png").unwrap();
        assert_eq!(card.3["kind"], json!("image"));
    }

    #[test]
    fn media_session_id_is_path_safe() {
        let plan = SendPlan {
            node: "a:b/c".into(),
            display: String::new(),
            refs: Vec::new(),
            images: Vec::new(),
            agent_id: None,
            model_pick: None,
            effort: None,
            origin: SendOrigin::Card,
            config: None,
        };
        let id = plan.media_session_id();
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn card_title_falls_back_to_first_user_message() {
        let mut node = atelyx_core::vault::CanvasFileNode {
            id: "n1".into(),
            node_type: "conversation".into(),
            x: 0.0,
            y: 0.0,
            width: None,
            height: None,
            data: json!({ "messages": [ { "id": "m1", "role": "user", "displayContent": "第一行\n第二行" } ] }),
        };
        assert_eq!(card_title(&node), "第一行");
        node.data["title"] = json!("话题");
        assert_eq!(card_title(&node), "话题");
    }
}
