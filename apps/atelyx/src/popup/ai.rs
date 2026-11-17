//! AI 弹层：模型 / 推理等级 / Agent / 命名与搜索源设置。

use crate::layout_view::{canvas_card_effort_label, canvas_card_model_label, card_effort, card_model_pick, chat_effort_label, chat_model_trigger_label, model_display, PopupState};
use crate::settings_view::ai;
use crate::ui::menu::MenuEntry;
use crate::{App, Message};

use super::common::popup_hint;

pub(super) fn entries<'a>(app: &'a App, state: &PopupState, pane: usize, window: iced::window::Id) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::AiModelMenu => {
            let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
            for p in app.repo_config.providers.iter().flatten() {
                if p.models.is_empty() {
                    continue;
                }
                entries.push((0, MenuEntry::Group(p.name.clone())));
                for m in &p.models {
                    let is_current = app
                        .repo_config
                        .auto_naming_model
                        .as_ref()
                        .is_some_and(|o| o.provider_id == p.id && o.model == m.id);
                    let pick =
                        Message::Ai(ai::AiMsg::NamingModelPick(Some((p.id.clone(), m.id.clone()))));
                    entries.push((0, MenuEntry::check(model_display(m), is_current, pick)));
                }
            }
            if entries.is_empty() {
                entries.push((0, MenuEntry::Custom(popup_hint("先在「供应商」添加模型"), 18.0)));
            }
            entries
        }
        PopupState::ChatAgentMenu { scope } => {
            // 未选过 = 内置「对话」生效；名册由装载时种子补齐，恒含「对话」
            let current = match scope {
                crate::chat_composer::ConfigScope::Panel => app
                    .chat
                    .active_agent_id()
                    .unwrap_or_else(|| atelyx_core::vault::BUILTIN_AGENT_CHAT_ID.to_string()),
                crate::chat_composer::ConfigScope::Card { node } => card_agent_id(app, node)
                    .unwrap_or_else(|| atelyx_core::vault::BUILTIN_AGENT_CHAT_ID.to_string()),
            };
            let pick = |id: String| match scope {
                crate::chat_composer::ConfigScope::Panel => Message::Chat(
                    crate::chat_actions::ChatMsg::AgentPick(Some(id)),
                ),
                crate::chat_composer::ConfigScope::Card { node } => Message::Canvas(
                    crate::canvas_view::CanvasMsg::Chat(crate::canvas_chat::CanvasChatMsg::AgentPick {
                        node: node.clone(),
                        agent: Some(id),
                    }),
                ),
            };
            let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
            for agent in &app.agents {
                let press = pick(agent.id.clone());
                entries.push((
                    0,
                    MenuEntry::check(agent.name.clone(), current == agent.id, press),
                ));
            }
            entries
        }
        PopupState::ChatPlusMenu { scope } => {
            let add_ctx = match scope {
                crate::chat_composer::ComposerScope::Panel => {
                    Message::Chat(crate::chat_actions::ChatMsg::TriggerMention)
                }
                crate::chat_composer::ComposerScope::Card { node } => Message::Canvas(
                    crate::canvas_view::CanvasMsg::Chat(
                        crate::canvas_chat::CanvasChatMsg::TriggerMention(node.clone()),
                    ),
                ),
            };
            let upload = match scope {
                crate::chat_composer::ComposerScope::Panel => {
                    Message::Chat(crate::chat_actions::ChatMsg::UploadAttachment)
                }
                crate::chat_composer::ComposerScope::Card { node } => Message::Canvas(
                    crate::canvas_view::CanvasMsg::Chat(
                        crate::canvas_chat::CanvasChatMsg::PickFiles(node.clone()),
                    ),
                ),
            };
            vec![
                (0, MenuEntry::action("添加上下文", add_ctx)),
                (0, MenuEntry::action("上传附件", upload)),
            ]
        }
        PopupState::ChatModelMenu { scope } => {
            // 两级菜单（pane 0 根 / 1 模型 / 2 推理等级）：模型与等级为正交覆盖。
            // 条目元组首元素是 pane 归属标签，pane_view 按当前 pane 过滤——必须与所在面板同值
            match pane {
                2 => {
                    let (current, pick): (
                        Option<String>,
                        Box<dyn Fn(Option<String>) -> Message>,
                    ) = match scope {
                        crate::chat_composer::ConfigScope::Panel => (
                            app.chat.effort.clone(),
                            Box::new(|v: Option<String>| {
                                Message::Chat(crate::chat_actions::ChatMsg::EffortPicked(v))
                            }),
                        ),
                        crate::chat_composer::ConfigScope::Card { node } => {
                            let node = node.clone();
                            (
                                card_effort(app, &node),
                                Box::new(move |v: Option<String>| {
                                    Message::Canvas(
                                        crate::canvas_view::CanvasMsg::Chat(
                                            crate::canvas_chat::CanvasChatMsg::EffortPick {
                                                node: node.clone(),
                                                effort: v,
                                            },
                                        ),
                                    )
                                }),
                            )
                        }
                    };
                    [
                        (None, "默认"),
                        (Some("off".to_string()), "关闭"),
                        (Some("low".to_string()), "低"),
                        (Some("medium".to_string()), "中"),
                        (Some("high".to_string()), "高"),
                    ]
                    .into_iter()
                    .map(|(value, label)| {
                        let pick = pick(value.clone());
                        let is_current = current == value;
                        (
                            2,
                            MenuEntry::check(label, is_current, pick),
                        )
                    })
                    .collect::<Vec<_>>()
                }
                1 => match scope {
                    crate::chat_composer::ConfigScope::Panel => {
                        chat_model_entries(app).into_iter().map(|(_, e)| (1, e)).collect()
                    }
                    crate::chat_composer::ConfigScope::Card { node } => canvas_card_model_entries(app, node)
                        .into_iter()
                        .map(|(_, e)| (1, e))
                        .collect(),
                },
                _ => {
                    let (model_label, effort_label) = match scope {
                        crate::chat_composer::ConfigScope::Panel => (
                            chat_model_trigger_label(app),
                            chat_effort_label(app),
                        ),
                        crate::chat_composer::ConfigScope::Card { node } => (
                            canvas_card_model_label(app, node),
                            canvas_card_effort_label(app, node),
                        ),
                    };
                    vec![
                        (
                            0,
                            MenuEntry::Submenu {
                                label: format!("模型：{model_label}"),
                                icon: None,
                                disabled: false,
                                pane: 1,
                            },
                        ),
                        (
                            0,
                            MenuEntry::Submenu {
                                label: format!("推理等级：{effort_label}"),
                                icon: None,
                                disabled: false,
                                pane: 2,
                            },
                        ),
                    ]
                }
            }
        }
        PopupState::AiNamingMenu => {
            let mode = ai::naming_mode(app);
            [
                (ai::NamingMode::Off, "不启用"),
                (ai::NamingMode::FollowDefault, "跟随当前模型"),
                (ai::NamingMode::Custom, "指定模型"),
            ]
            .into_iter()
            .map(|(m, label)| {
                let pick = Message::Ai(ai::AiMsg::NamingModePick(m));
                (0, MenuEntry::check(label, mode == m, pick))
            })
            .collect()
        }
        PopupState::AiDefaultModelMenu => {
            let current = app
                .repo_config
                .model
                .as_deref()
                .zip(app.repo_config.model_provider_id.as_deref());
            let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
            for provider in app.repo_config.providers.iter().flatten() {
                for model in &provider.models {
                    let picked = matches!(
                        current,
                        Some((current_model, current_provider))
                            if current_provider == provider.id && current_model == model.id
                    );
                    let label = model.nickname.as_deref().unwrap_or(&model.id);
                    entries.push((
                        0,
                        MenuEntry::check(
                            format!("{} / {label}", provider.name),
                            picked,
                            Message::Ai(ai::AiMsg::DefaultModelPick(
                                provider.id.clone(),
                                model.id.clone(),
                            )),
                        ),
                    ));
                }
            }
            if entries.is_empty() {
                entries.push((0, MenuEntry::Custom(popup_hint("先在「供应商」添加模型"), 18.0)));
            }
            entries
        }
        PopupState::AiAgentPromptMenu => {
            // 弹层由 Agent 表单触发：当前值以草稿优先（可能含未保存的改动）
            let current = app.ai.agent_draft.as_ref().and_then(|d| d.prompt_file.clone());
            let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![(
                0,
                MenuEntry::check("无", current.is_none(), Message::Ai(ai::AiMsg::AgentPromptPick(None))),
            )];
            for rel in &app.prompt_notes {
                let label = rel.rsplit('/').next().unwrap_or(rel).trim_end_matches(".md").to_string();
                let is_current = current.as_deref() == Some(rel.as_str());
                let pick = Message::Ai(ai::AiMsg::AgentPromptPick(Some(rel.clone())));
                entries.push((0, MenuEntry::check(label, is_current, pick)));
            }
            if app.prompt_notes.is_empty() {
                entries.push((0, MenuEntry::Custom(popup_hint("暂无提示词笔记（文件右键「注册为提示词」）"), 18.0)));
            }
            entries
        }
        PopupState::AiProviderDeleteMenu => {
            vec![
                (
                    0,
                    MenuEntry::Submenu { label: "删除供应商".into(), icon: Some("bin".into()), disabled: false, pane: 1 },
                ),
                (
                    1,
                    MenuEntry::danger("确认删除", "bin", Message::Ai(ai::AiMsg::ProviderDelete)),
                ),
                (1, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))),
            ]
        }
        PopupState::AiAgentDeleteMenu { agent_id } => {
            // 删除目标随弹层携带：悬停行与当前选中行可以不同，不能按选中项删
            let name = app
                .agents
                .iter()
                .find(|a| &a.id == agent_id)
                .map(|a| a.name.chars().take(10).collect::<String>())
                .unwrap_or_default();
            let confirm = if name.is_empty() {
                "确认删除".to_string()
            } else {
                format!("删除「{name}」")
            };
            vec![
                (
                    0,
                    MenuEntry::Submenu { label: "删除 Agent".into(), icon: Some("bin".into()), disabled: false, pane: 1 },
                ),
                (
                    1,
                    MenuEntry::danger(
                        confirm,
                        "bin",
                        Message::Ai(ai::AiMsg::AgentDelete(agent_id.clone())),
                    ),
                ),
                (1, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))),
            ]
        }
        PopupState::AiSearchProviderMenu => {
            let current = app
                .ai
                .search_draft
                .as_ref()
                .map(|d| d.provider.clone())
                .or_else(|| app.repo_config.search.as_ref().map(|s| s.provider.clone()))
                .unwrap_or_default();
            [("tavily", "Tavily"), ("searxng", "SearXNG")]
                .into_iter()
                .map(|(key, label)| {
                    // 空值 = 未配置，缺省语义按 Tavily 展示
                    let is_current = current == key || (current.is_empty() && key == "tavily");
                    let pick = Message::Ai(ai::AiMsg::SearchProviderPick(key.to_string()));
                    (0, MenuEntry::check(label, is_current, pick))
                })
                .collect()
        }
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        // AI 设置：模型 id / 笔记名可能较长，给足一行的宽度
        PopupState::AiModelMenu => 224.0,
        PopupState::ChatModelMenu { .. } => 224.0,
        PopupState::ChatAgentMenu { .. } => 200.0,
        PopupState::ChatPlusMenu { .. } => 176.0,
        PopupState::AiNamingMenu => 144.0,
        PopupState::AiDefaultModelMenu => 208.0,
        PopupState::AiAgentPromptMenu => 208.0,
        PopupState::AiProviderDeleteMenu | PopupState::AiAgentDeleteMenu { .. } => 208.0,
        PopupState::AiSearchProviderMenu => 144.0,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}

/// 卡节点 Agent id（`data.agentId`）。
fn card_agent_id(app: &App, node_id: &str) -> Option<String> {
    app.canvas
        .as_ref()
        .and_then(|s| s.data.nodes.iter().find(|n| n.id == node_id))
        .and_then(|n| n.data.get("agentId").and_then(serde_json::Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 卡模型子面板条目：跟随默认（卡有指定时）+ 按供应商分组的模型清单。
fn canvas_card_model_entries(app: &App, node_id: &str) -> Vec<(usize, MenuEntry<'static, Message>)> {
    use crate::canvas_chat::CanvasChatMsg;
    use crate::canvas_view::CanvasMsg;
    let pick = |p: String, m: String| {
        Message::Canvas(CanvasMsg::Chat(CanvasChatMsg::ModelPick {
            node: node_id.to_string(),
            pick: Some((p, m)),
        }))
    };
    let current = card_model_pick(app, node_id);
    // 卡已指定模型时提供「跟随默认」（移除节点级指定）
    let card_has_model = app
        .canvas
        .as_ref()
        .and_then(|s| s.data.nodes.iter().find(|n| n.id == node_id))
        .is_some_and(|n| {
            n.data.get("model").and_then(serde_json::Value::as_str).is_some_and(|m| !m.is_empty())
        });
    let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = Vec::new();
    if card_has_model {
        entries.push((
            0,
            MenuEntry::action(
                "跟随当前模型",
                Message::Canvas(CanvasMsg::Chat(CanvasChatMsg::ModelPick {
                    node: node_id.to_string(),
                    pick: None,
                })),
            ),
        ));
    }
    for p in app.repo_config.providers.iter().flatten() {
        if p.models.is_empty() {
            continue;
        }
        entries.push((0, MenuEntry::Group(p.name.clone())));
        for m in &p.models {
            let is_current =
                current.as_ref().is_some_and(|(provider, model)| provider == &p.id && model == &m.id);
            let press = pick(p.id.clone(), m.id.clone());
            entries.push((0, MenuEntry::check(model_display(m), is_current, press)));
        }
    }
    if entries.len() == usize::from(card_has_model) {
        entries.push((0, MenuEntry::Custom(popup_hint("先在「供应商」添加模型"), 18.0)));
    }
    entries
}

/// 面板模型子面板条目：按供应商分组，昵称显示。
fn chat_model_entries(app: &App) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let current = app.chat.model_override.as_ref().and_then(|v| {
        let provider = v.get("providerId").and_then(serde_json::Value::as_str)?;
        let model = v.get("model").and_then(serde_json::Value::as_str)?;
        Some((provider.to_string(), model.to_string()))
    });
    let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = Vec::new();
    for p in app.repo_config.providers.iter().flatten() {
        if p.models.is_empty() {
            continue;
        }
        entries.push((0, MenuEntry::Group(p.name.clone())));
        for m in &p.models {
            let is_current = current
                .as_ref()
                .is_some_and(|(provider, model)| provider == &p.id && model == &m.id);
            let pick = Message::Chat(crate::chat_actions::ChatMsg::ModelPicked(Some((
                p.id.clone(),
                m.id.clone(),
            ))));
            entries.push((0, MenuEntry::check(model_display(m), is_current, pick)));
        }
    }
    if entries.is_empty() {
        entries.push((0, MenuEntry::Custom(popup_hint("先在「供应商」添加模型"), 18.0)));
    }
    entries
}
