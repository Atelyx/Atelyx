//! 设置-AI：供应商 / 模型服务 / Agent / 联网搜索（仓库级，本地落 `.atelyx/`，空间落服务端）。
//! 编辑走草稿 + 保存；API key 缺省只进密钥链，`syncKeys` 开启才随仓库落盘（切换落点先迁移不丢 key）。

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Padding, Point, Task, border, widget::container::Style};

use atelyx_core::credentials;
use atelyx_core::vault;

use crate::layout_view::{PopupOpen, PopupState, mono};
use crate::space_actions;
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::{App, Message};

/// 表单输入宽度。
const FORM_W: f32 = 240.0;
/// 左列（供应商 / Agent 列表）宽度。
const LIST_W: f32 = 220.0;

/// 密钥链条目尾：Tavily key（对齐 config 注释里的 `provider-<sha>-search-tavily`）。
const TAVILY_TAIL: &str = "search-tavily";

/// 供应商预设（添加菜单；预设自带默认模型清单）。
pub(crate) struct ProviderPreset {
    pub name: &'static str,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
}

pub(crate) const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset { name: "OpenAI", base_url: "https://api.openai.com/v1", models: &["gpt-4o-mini", "gpt-4o"] },
    ProviderPreset { name: "DeepSeek", base_url: "https://api.deepseek.com/v1", models: &["deepseek-chat", "deepseek-reasoner"] },
    ProviderPreset {
        name: "通义千问",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        models: &["qwen-plus", "qwen-max"],
    },
];

/// 工具四分类（id 与 AI 工具注册名一致；空清单 = 纯对话）。
struct ToolCategory {
    label: &'static str,
    tools: &'static [&'static str],
}

const TOOL_CATEGORIES: &[ToolCategory] = &[
    ToolCategory { label: "联网", tools: &["web_search", "web_fetch"] },
    ToolCategory {
        label: "文件",
        tools: &[
            "read_file", "glob", "grep", "list_dir", "read_history", "edit_file", "append_file",
            "write_file", "rename_file", "move_file", "delete_file", "delete_dir",
        ],
    },
    ToolCategory { label: "任务清单", tools: &["todo_write"] },
    ToolCategory { label: "插件", tools: &["plugin"] },
];

/// 话题自动命名模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamingMode {
    Off,
    FollowDefault,
    Custom,
}

/// 当前命名模式（enabled 缺省 = 开启；model 有值 = 指定）。
pub(crate) fn naming_mode(app: &App) -> NamingMode {
    if !app.repo_config.auto_naming_enabled.unwrap_or(true) {
        return NamingMode::Off;
    }
    if app.repo_config.auto_naming_model.is_some() {
        NamingMode::Custom
    } else {
        NamingMode::FollowDefault
    }
}

// ===== 会话内编辑状态 =====

/// 设置-AI 的会话内编辑状态（选中项与草稿；容器切换时整体清空）。
#[derive(Default)]
pub struct AiState {
    pub provider_sel: Option<String>,
    pub provider_draft: Option<ProviderDraft>,
    pub agent_sel: Option<String>,
    pub agent_draft: Option<AgentDraft>,
    pub search_draft: Option<SearchDraft>,
    /// 连通性测试在途（防重入）。
    pub testing: bool,
    /// 待清理的密钥链条目 (scope, 供应商 id)：供应商删除在配置落盘成功后执行。
    pub provider_key_delete_pending: Option<(String, String)>,
}

/// 供应商表单草稿（api_key 明文仅存内存；保存按 syncKeys 决定落点）。
#[derive(Clone)]
pub struct ProviderDraft {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub api_key: String,
    pub sync_keys: bool,
}

/// Agent 表单草稿。
#[derive(Clone)]
pub struct AgentDraft {
    pub id: String,
    pub name: String,
    pub prompt_file: Option<String>,
    pub tools: Vec<String>,
}

/// 搜索源表单草稿（tavily_key 不回显已存值，留空 = 不改）。
#[derive(Clone)]
pub struct SearchDraft {
    pub provider: String,
    pub tavily_key: String,
    pub searxng_url: String,
}

#[derive(Debug, Clone)]
pub enum AiMsg {
    SubTab(&'static str),
    // 供应商
    ProviderSelect(String),
    ProviderAddPreset(usize),
    ProviderAddCustom,
    ProviderName(String),
    ProviderUrl(String),
    ProviderModel(usize, String),
    ProviderModelAdd,
    ProviderModelRemove(usize),
    ProviderKey(String),
    ProviderSyncToggle,
    ProviderSave,
    ProviderTest,
    ProviderTestDone(u64, Result<(), String>),
    ProviderDelete,
    // 模型服务
    DefaultModelPick(Option<(String, String)>),
    NamingModePick(NamingMode),
    NamingModelPick(Option<(String, String)>),
    // Agent
    AgentSelect(String),
    AgentNew,
    AgentName(String),
    AgentPromptPick(Option<String>),
    AgentToolToggle(String),
    /// 整类开关（TOOL_CATEGORIES 下标）：全开 → 全关，否则全开。
    AgentCategoryToggle(usize),
    AgentSave,
    AgentDelete,
    // 联网搜索
    SearchProviderPick(String),
    SearchTavilyKey(String),
    SearchSearxUrl(String),
    SearchSave,
}

// ===== 入口 =====

pub fn view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    if super::repo_display_name(app).is_none() {
        return container(controls::empty_state("bot", "未打开仓库", "打开仓库后配置", None))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }
    let mut col = column![].spacing(8).padding(Padding::ZERO.horizontal(20.0));
    col = col.push(sub_tab_bar(app));
    if readonly(app) {
        col = col.push(read_only_banner());
    }
    col = col.push(match app.ai_sub_tab.as_str() {
        "models" => models_view(app, window),
        "agents" => agents_view(app, window),
        "search" => search_view(app, window),
        _ => providers_view(app, window),
    });
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Agent 配置读取（打开 AI 设置页时；本地读文件，空间走服务端）。
pub fn load_agents(app: &App) -> Task<Message> {
    if let crate::RepoKind::Space(_) = app.repo_kind {
        return space_actions::space_load_agents(app);
    }
    let Some(root) = app.vault_root.clone() else { return Task::none() };
    let gen = app.epoch();
    Task::perform(
        async move { vault::read_agents_file(&root) },
        move |result| Message::AgentsLoaded(gen, result),
    )
}

/// agents.json 写盘（本地 = 原子写后重读；空间 = 整表 PUT 后重读；回执经 AgentsLoaded）。
fn save_agents(app: &mut App, agents: Vec<vault::AgentConfig>) -> Task<Message> {
    if let crate::RepoKind::Space(_) = app.repo_kind {
        return space_actions::space_write_agents(app, agents);
    }
    let Some(root) = app.vault_root.clone() else { return Task::none() };
    let gen = app.epoch();
    Task::perform(
        async move {
            vault::write_agents_file(&root, &agents)?;
            vault::read_agents_file(&root)
        },
        move |result| Message::AgentsLoaded(gen, result),
    )
}

/// 密钥链条目 scope：本地 = 仓库根路径；空间 = 「服务器地址/空间 id」。
fn key_scope(app: &App) -> Option<String> {
    match &app.repo_kind {
        crate::RepoKind::Local => app.vault_root.as_ref().map(|r| r.to_string_lossy().into_owned()),
        crate::RepoKind::Space(s) => Some(format!("{}/{}", s.server_url, s.space_id)),
    }
}

/// 空间 viewer 只读（写控件降级 + 写路径拦截）。
fn readonly(app: &App) -> bool {
    matches!(&app.repo_kind, crate::RepoKind::Space(s) if !s.role.can_write())
}

/// 光标锚定的弹层打开消息（表单下拉统一入口）。
fn open_popup(app: &App, window: iced::window::Id, state: PopupState) -> Message {
    let cursor = app.cursor_for(window);
    Message::PopupOpen(
        window,
        PopupOpen {
            point: Point::new(cursor.x, cursor.y + 16.0),
            flip_y: Some(cursor.y - 12.0),
            state,
            min_width: 0.0,
        },
    )
}

fn sub_tab_bar(app: &App) -> Element<'static, Message> {
    const TABS: [(&str, &str); 4] =
        [("providers", "供应商"), ("models", "模型服务"), ("agents", "Agent"), ("search", "联网搜索")];
    controls::segmented_tabs(&TABS, &app.ai_sub_tab, |id| Message::Ai(AiMsg::SubTab(id)))
}

fn read_only_banner() -> Element<'static, Message> {
    container(
        row![
            icon("info", 13.0, pal::TEXT_MUTED),
            text("当前角色为只读，无法修改 AI 设置").size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(6),
    )
    .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
    .width(Length::Fill)
    .style(|_t| Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: border::rounded(pal::RADIUS_SM),
        ..Style::default()
    })
    .into()
}

/// 只读态的输入替身（form_input 无禁用形态：以纯文本展示当前值）。
fn value_text(v: &str) -> Element<'static, Message> {
    container(text(v.to_string()).size(pal::fs_caption()).color(pal::TEXT_SECONDARY))
        .width(Length::Fixed(FORM_W))
        .padding(Padding::ZERO.vertical(6.0))
        .into()
}

// ===== 消息处理 =====

pub fn update(app: &mut App, msg: AiMsg) -> Task<Message> {
    match msg {
        AiMsg::SubTab(id) => {
            app.ai_sub_tab = id.to_string();
            if id == "agents" {
                return load_agents(app);
            }
            Task::none()
        }

        // ---- 供应商 ----
        AiMsg::ProviderSelect(id) => {
            app.ai.provider_sel = Some(id.clone());
            app.ai.provider_draft = app
                .repo_config
                .providers
                .iter()
                .flatten()
                .find(|p| p.id == id)
                .map(|p| provider_draft_of(app, p));
            Task::none()
        }
        AiMsg::ProviderAddPreset(i) => {
            app.dismiss_popup();
            let Some(p) = PROVIDER_PRESETS.get(i) else { return Task::none() };
            add_provider(
                app,
                p.name,
                p.base_url,
                p.models.iter().map(|m| m.to_string()).collect(),
            )
        }
        AiMsg::ProviderAddCustom => {
            app.dismiss_popup();
            add_provider(app, "自定义", "", Vec::new())
        }
        AiMsg::ProviderName(v) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.name = v;
            }
            Task::none()
        }
        AiMsg::ProviderUrl(v) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.base_url = v;
            }
            Task::none()
        }
        AiMsg::ProviderModel(i, v) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                if let Some(m) = d.models.get_mut(i) {
                    *m = v;
                }
            }
            Task::none()
        }
        AiMsg::ProviderModelAdd => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.models.push(String::new());
            }
            Task::none()
        }
        AiMsg::ProviderModelRemove(i) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.models.remove(i);
            }
            Task::none()
        }
        AiMsg::ProviderKey(v) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.api_key = v;
            }
            Task::none()
        }
        AiMsg::ProviderSyncToggle => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.sync_keys = !d.sync_keys;
            }
            Task::none()
        }
        AiMsg::ProviderSave => {
            let Some(draft) = app.ai.provider_draft.clone() else { return Task::none() };
            save_provider(app, draft)
        }
        AiMsg::ProviderTest => {
            if app.ai.testing || readonly(app) {
                return Task::none();
            }
            let (base_url, key) = provider_test_inputs(app);
            if base_url.trim().is_empty() {
                app.notify(NoticeLevel::Warning, "先填写 baseUrl");
                return Task::none();
            }
            app.ai.testing = true;
            let gen = app.epoch();
            Task::perform(
                async move { atelyx_core::ai_provider::test_endpoint(&base_url, &key).await },
                move |result| Message::Ai(AiMsg::ProviderTestDone(gen, result)),
            )
        }
        AiMsg::ProviderTestDone(gen, result) => {
            if gen != app.epoch() {
                return Task::none();
            }
            app.ai.testing = false;
            match result {
                Ok(()) => app.notify(NoticeLevel::Success, "连接成功"),
                Err(e) => app.notify(NoticeLevel::Error, format!("连接失败：{e}")),
            }
            Task::none()
        }
        AiMsg::ProviderDelete => {
            app.dismiss_popup();
            if readonly(app) {
                return Task::none();
            }
            let Some(id) = app.ai.provider_sel.take() else { return Task::none() };
            app.ai.provider_draft = None;
            let mut providers = app.repo_config.providers.take().unwrap_or_default();
            providers.retain(|p| p.id != id);
            app.repo_config.providers = Some(providers.clone());
            let Ok(list) = serde_json::to_value(&providers) else {
                app.notify(NoticeLevel::Error, "供应商配置序列化失败");
                return Task::none();
            };
            // 密钥链条目挂到配置落盘回执清理（写失败保留密钥，恢复供应商后仍可用）
            app.ai.provider_key_delete_pending = key_scope(app).map(|scope| (scope, id));
            app.patch_repo_config(serde_json::json!({ "providers": list }))
        }

        // ---- 模型服务 ----
        AiMsg::DefaultModelPick(pick) => {
            app.dismiss_popup();
            let (model, provider_id) = match pick {
                Some((pid, mid)) => (Some(mid.clone()), Some(pid)),
                None => (None, None),
            };
            app.repo_config.model = model;
            app.repo_config.model_provider_id = provider_id;
            app.patch_repo_config(serde_json::json!({
                "model": app.repo_config.model,
                "modelProviderId": app.repo_config.model_provider_id,
            }))
        }
        AiMsg::NamingModePick(mode) => {
            app.dismiss_popup();
            let enabled = mode != NamingMode::Off;
            app.repo_config.auto_naming_enabled = Some(enabled);
            let mut patch = serde_json::json!({ "autoNamingEnabled": enabled });
            if mode != NamingMode::Custom {
                // 不启用 / 跟随默认都清掉指定模型（缺省 = 跟随默认模型）
                app.repo_config.auto_naming_model = None;
                patch["autoNamingModel"] = serde_json::Value::Null;
            }
            app.patch_repo_config(patch)
        }
        AiMsg::NamingModelPick(pick) => {
            app.dismiss_popup();
            let (Some((pid, mid)), true) = (pick, app.repo_config.auto_naming_enabled.unwrap_or(true)) else {
                return Task::none();
            };
            app.repo_config.auto_naming_model = Some(vault::EditorChatModelOverride {
                provider_id: pid.clone(),
                model: mid.clone(),
            });
            app.patch_repo_config(serde_json::json!({
                "autoNamingModel": { "providerId": pid, "model": mid },
            }))
        }

        // ---- Agent ----
        AiMsg::AgentSelect(id) => {
            app.ai.agent_sel = Some(id.clone());
            app.ai.agent_draft = app.agents.iter().find(|a| a.id == id).map(agent_draft_of);
            Task::none()
        }
        AiMsg::AgentNew => {
            if readonly(app) {
                return Task::none();
            }
            let id = format!("agent-{}", nanoid::nanoid!(8));
            let mut agents = app.agents.clone();
            agents.push(vault::AgentConfig {
                id: id.clone(),
                name: "新 Agent".into(),
                system_prompt_file: None,
                tools: Vec::new(),
                builtin: None,
            });
            app.agents = agents.clone();
            app.ai.agent_sel = Some(id.clone());
            app.ai.agent_draft = Some(AgentDraft {
                id,
                name: "新 Agent".into(),
                prompt_file: None,
                tools: Vec::new(),
            });
            save_agents(app, agents)
        }
        AiMsg::AgentName(v) => {
            if let Some(d) = app.ai.agent_draft.as_mut() {
                d.name = v;
            }
            Task::none()
        }
        AiMsg::AgentPromptPick(rel) => {
            app.dismiss_popup();
            if let Some(d) = app.ai.agent_draft.as_mut() {
                d.prompt_file = rel;
            }
            Task::none()
        }
        AiMsg::AgentToolToggle(tool) => {
            if let Some(d) = app.ai.agent_draft.as_mut() {
                if let Some(i) = d.tools.iter().position(|t| t == &tool) {
                    d.tools.remove(i);
                } else {
                    d.tools.push(tool);
                }
            }
            Task::none()
        }
        AiMsg::AgentCategoryToggle(i) => {
            let Some(cat) = TOOL_CATEGORIES.get(i) else { return Task::none() };
            if let Some(d) = app.ai.agent_draft.as_mut() {
                let all_on = cat.tools.iter().all(|t| d.tools.iter().any(|x| x == t));
                if all_on {
                    d.tools.retain(|x| !cat.tools.contains(&x.as_str()));
                } else {
                    for t in cat.tools {
                        if !d.tools.iter().any(|x| x == t) {
                            d.tools.push(t.to_string());
                        }
                    }
                }
            }
            Task::none()
        }
        AiMsg::AgentSave => {
            let Some(draft) = app.ai.agent_draft.clone() else { return Task::none() };
            if readonly(app) {
                return Task::none();
            }
            let mut agents = app.agents.clone();
            let Some(a) = agents.iter_mut().find(|a| a.id == draft.id) else {
                app.notify(NoticeLevel::Warning, "Agent 已被删除");
                return Task::none();
            };
            a.name = draft.name.trim().to_string();
            a.system_prompt_file = draft.prompt_file;
            a.tools = draft.tools;
            app.agents = agents.clone();
            save_agents(app, agents)
        }
        AiMsg::AgentDelete => {
            app.dismiss_popup();
            if readonly(app) {
                return Task::none();
            }
            let Some(id) = app.ai.agent_sel.take() else { return Task::none() };
            app.ai.agent_draft = None;
            let mut agents = app.agents.clone();
            agents.retain(|a| a.id != id);
            app.agents = agents.clone();
            save_agents(app, agents)
        }

        // ---- 联网搜索 ----
        AiMsg::SearchProviderPick(v) => {
            ensure_search_draft(app).provider = v;
            Task::none()
        }
        AiMsg::SearchTavilyKey(v) => {
            ensure_search_draft(app).tavily_key = v;
            Task::none()
        }
        AiMsg::SearchSearxUrl(v) => {
            ensure_search_draft(app).searxng_url = v;
            Task::none()
        }
        AiMsg::SearchSave => {
            let Some(draft) = app.ai.search_draft.clone() else { return Task::none() };
            save_search(app, draft)
        }
    }
}

fn provider_draft_of(app: &App, p: &vault::VaultProvider) -> ProviderDraft {
    let sync = app.repo_config.sync_keys.unwrap_or(false);
    ProviderDraft {
        id: p.id.clone(),
        name: p.name.clone(),
        base_url: p.base_url.clone(),
        models: p.models.iter().map(|m| m.id.clone()).collect(),
        // 关同步不回显已存密钥（密钥链读取放测试/保存时按需取）
        api_key: sync.then(|| p.api_key.clone()).flatten().unwrap_or_default(),
        sync_keys: sync,
    }
}

fn agent_draft_of(a: &vault::AgentConfig) -> AgentDraft {
    AgentDraft {
        id: a.id.clone(),
        name: a.name.clone(),
        prompt_file: a.system_prompt_file.clone(),
        tools: a.tools.clone(),
    }
}

/// 搜索草稿懒建（首个输入/选择动作为其落草稿；基线取当前配置）。
fn ensure_search_draft(app: &mut App) -> &mut SearchDraft {
    if app.ai.search_draft.is_none() {
        let s = app.repo_config.search.clone().unwrap_or_default();
        app.ai.search_draft = Some(SearchDraft {
            provider: if s.provider.is_empty() { "tavily".into() } else { s.provider },
            tavily_key: String::new(),
            searxng_url: s.searxng_url,
        });
    }
    app.ai.search_draft.as_mut().expect("上一行刚置值")
}

/// 新增供应商：预设/空模板即刻入列并写盘（默认无 key），选中进入编辑。
fn add_provider(app: &mut App, name: &str, base_url: &str, models: Vec<String>) -> Task<Message> {
    if readonly(app) {
        return Task::none();
    }
    let id = format!("prov-{}", nanoid::nanoid!(8));
    let mut providers = app.repo_config.providers.clone().unwrap_or_default();
    providers.push(vault::VaultProvider {
        id: id.clone(),
        name: name.to_string(),
        base_url: base_url.to_string(),
        models: models.iter().map(|m| vault::VaultModel { id: m.clone(), nickname: None }).collect(),
        api_key: None,
    });
    let Ok(list) = serde_json::to_value(&providers) else {
        app.notify(NoticeLevel::Error, "供应商配置序列化失败");
        return Task::none();
    };
    app.repo_config.providers = Some(providers);
    app.ai.provider_sel = Some(id.clone());
    app.ai.provider_draft = Some(ProviderDraft {
        id,
        name: name.to_string(),
        base_url: base_url.to_string(),
        models,
        api_key: String::new(),
        sync_keys: app.repo_config.sync_keys.unwrap_or(false),
    });
    let sync = app.repo_config.sync_keys.unwrap_or(false);
    app.patch_repo_config(serde_json::json!({ "providers": list, "syncKeys": sync }))
}

/// 供应商保存：字段入列 + 按 syncKeys 决定 key 落点（关同步 = 全部明文先迁密钥链再剥离）。
fn save_provider(app: &mut App, draft: ProviderDraft) -> Task<Message> {
    if readonly(app) {
        return Task::none();
    }
    let Some(scope) = key_scope(app) else { return Task::none() };
    let mut providers = app.repo_config.providers.clone().unwrap_or_default();
    let Some(index) = providers.iter().position(|p| p.id == draft.id) else {
        app.notify(NoticeLevel::Warning, "供应商已被删除");
        return Task::none();
    };
    let models: Vec<String> = draft
        .models
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let sync = draft.sync_keys;
    let entry = &mut providers[index];
    entry.name = draft.name.trim().to_string();
    entry.base_url = draft.base_url.trim().to_string();
    entry.models = models.into_iter().map(|m| vault::VaultModel { id: m, nickname: None }).collect();
    entry.api_key = sync.then(|| draft.api_key.clone()).filter(|k| !k.is_empty());

    // 关同步 = key 只进密钥链：补丁按数组整体替换，所有明文（含其他供应商与搜索源的）
    // 先迁密钥链再剥离，任一迁移失败即取消保存（不丢 key）
    if !sync {
        if !draft.api_key.is_empty() {
            if let Err(e) = credentials::store_provider_key(&scope, &draft.id, &draft.api_key) {
                app.notify(NoticeLevel::Error, format!("API key 写入密钥链失败，已取消保存：{e}"));
                return Task::none();
            }
        }
        for e in providers.iter_mut() {
            if let Some(old) = e.api_key.take() {
                if old.is_empty() {
                    continue;
                }
                if let Err(err) = credentials::store_provider_key(&scope, &e.id, &old) {
                    app.notify(NoticeLevel::Error, format!("API key 迁移密钥链失败，已取消保存：{err}"));
                    return Task::none();
                }
            }
        }
        if let Some(old) = app
            .repo_config
            .search
            .as_ref()
            .and_then(|s| s.tavily_api_key.clone())
            .filter(|k| !k.is_empty())
        {
            if let Err(err) = credentials::store_provider_key(&scope, TAVILY_TAIL, &old) {
                app.notify(NoticeLevel::Error, format!("API key 迁移密钥链失败，已取消保存：{err}"));
                return Task::none();
            }
        }
    }
    let Ok(list) = serde_json::to_value(&providers) else {
        app.notify(NoticeLevel::Error, "供应商配置序列化失败");
        return Task::none();
    };
    app.repo_config.providers = Some(providers);
    app.repo_config.sync_keys = Some(sync);
    let mut patch = serde_json::json!({ "providers": list, "syncKeys": sync });
    if !sync {
        // search 的明文 key 一并删键（null = 删键；guard 保住该指令不被同步态吞掉）
        patch["search"] = serde_json::json!({ "tavilyApiKey": serde_json::Value::Null });
    }
    app.patch_repo_config(patch)
}

/// 连通性测试的取值：baseUrl/key 以草稿优先（未保存的改动即刻可测），
/// key 草稿为空时按当前落点取值（开同步 = 配置明文；关同步 = 密钥链）。
fn provider_test_inputs(app: &App) -> (String, String) {
    let Some(sel) = app.ai.provider_sel.clone() else { return (String::new(), String::new()) };
    let Some(p) = app.repo_config.providers.iter().flatten().find(|p| p.id == sel) else {
        return (String::new(), String::new());
    };
    let draft = app.ai.provider_draft.as_ref().filter(|d| d.id == p.id);
    let base_url = draft.map(|d| d.base_url.clone()).unwrap_or_else(|| p.base_url.clone());
    let sync = draft.map(|d| d.sync_keys).unwrap_or_else(|| app.repo_config.sync_keys.unwrap_or(false));
    let key = match draft {
        Some(d) if !d.api_key.is_empty() => d.api_key.clone(),
        _ if sync => p.api_key.clone().unwrap_or_default(),
        // 密钥链读取（快速本机调用；读不到按空发起，由端点 401 反馈）
        _ => key_scope(app)
            .and_then(|scope| credentials::load_provider_key(&scope, &p.id).ok())
            .flatten()
            .unwrap_or_default(),
    };
    (base_url, key)
}

/// 搜索源保存：provider/URL 入列；tavily key 按 syncKeys 落点（规则同供应商）。
fn save_search(app: &mut App, draft: SearchDraft) -> Task<Message> {
    if readonly(app) {
        return Task::none();
    }
    let Some(scope) = key_scope(app) else { return Task::none() };
    let sync = app.repo_config.sync_keys.unwrap_or(false);
    let old_key = app
        .repo_config
        .search
        .as_ref()
        .and_then(|s| s.tavily_api_key.clone())
        .filter(|k| !k.is_empty());
    if !sync {
        // 关同步：既有明文迁密钥链（重复值覆盖同条目，无副作用），草稿新 key 入密钥链
        if let Some(old) = &old_key {
            if let Err(e) = credentials::store_provider_key(&scope, TAVILY_TAIL, old) {
                app.notify(NoticeLevel::Error, format!("API key 迁移密钥链失败，已取消保存：{e}"));
                return Task::none();
            }
        }
        if !draft.tavily_key.is_empty() {
            if let Err(e) = credentials::store_provider_key(&scope, TAVILY_TAIL, &draft.tavily_key) {
                app.notify(NoticeLevel::Error, format!("API key 写入密钥链失败，已取消保存：{e}"));
                return Task::none();
            }
        }
    }
    let search = vault::VaultSearchConfig {
        provider: draft.provider.trim().to_string(),
        searxng_url: draft.searxng_url.trim().to_string(),
        // 开同步：草稿填了才替换（空 = 保留现值）；关同步一律剥离
        tavily_api_key: if sync {
            (!draft.tavily_key.is_empty()).then(|| draft.tavily_key.clone()).or(old_key)
        } else {
            None
        },
    };
    let Ok(mut value) = serde_json::to_value(&search) else {
        app.notify(NoticeLevel::Error, "搜索配置序列化失败");
        return Task::none();
    };
    if !sync {
        // 显式删键（对象合并保留未提及字段；guard 保住该指令不被同步态吞掉）
        value["tavilyApiKey"] = serde_json::Value::Null;
    }
    app.repo_config.search = Some(search);
    app.patch_repo_config(serde_json::json!({ "search": value }))
}

// ===== 供应商 =====

fn providers_view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let providers: Vec<&vault::VaultProvider> = app.repo_config.providers.iter().flatten().collect();

    let mut list = column![].spacing(4);
    for p in &providers {
        list = list.push(provider_card(p, app.ai.provider_sel.as_deref() == Some(p.id.as_str())));
    }
    if providers.is_empty() {
        list = list.push(
            text("暂无供应商，点「添加」开始").size(pal::fs_caption()).color(pal::TEXT_MUTED),
        );
    }
    let add_btn = controls::btn(
        "添加",
        &controls::btn_sm(),
        controls::Variant::Secondary,
        (!ro).then(|| open_popup(app, window, PopupState::AiProviderAddMenu)),
    );

    let right: Element<'static, Message> = match app
        .ai
        .provider_sel
        .as_ref()
        .and_then(|id| providers.iter().find(|p| &p.id == id))
    {
        None => container(text("选择或添加供应商").size(pal::fs_caption()).color(pal::TEXT_MUTED))
            .width(Length::Fill)
            .padding(Padding::ZERO.top(24.0))
            .into(),
        Some(p) => provider_form(app, p, window),
    };

    row![
        container(column![list, space().height(Length::Fixed(8.0)), add_btn].width(Length::Fill))
            .width(Length::Fixed(LIST_W)),
        container(right).width(Length::Fill).padding(Padding::ZERO.left(20.0)),
    ]
    .into()
}

fn provider_card(p: &vault::VaultProvider, selected: bool) -> Element<'static, Message> {
    button(
        column![
            text(p.name.clone())
                .size(pal::fs_caption())
                .color(if selected { pal::ACCENT } else { pal::TEXT_PRIMARY }),
            text(if p.base_url.is_empty() { "未配置 baseUrl".to_string() } else { p.base_url.clone() })
                .size(pal::fs_micro())
                .color(pal::TEXT_MUTED),
        ]
        .spacing(2),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
    .style(controls::row_style(selected, pal::RADIUS_SM))
    .on_press(Message::Ai(AiMsg::ProviderSelect(p.id.clone())))
    .into()
}

fn provider_form(app: &App, p: &vault::VaultProvider, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let draft = app.ai.provider_draft.as_ref().filter(|d| d.id == p.id);
    let name = draft.map(|d| d.name.clone()).unwrap_or_else(|| p.name.clone());
    let base_url = draft.map(|d| d.base_url.clone()).unwrap_or_else(|| p.base_url.clone());
    let models = draft.map(|d| d.models.clone()).unwrap_or_else(|| p.models.iter().map(|m| m.id.clone()).collect());
    let key = draft.map(|d| d.api_key.clone()).unwrap_or_default();
    let sync = draft.map(|d| d.sync_keys).unwrap_or_else(|| app.repo_config.sync_keys.unwrap_or(false));

    let name_control = if ro {
        value_text(&name)
    } else {
        controls::form_input("名称", &name, FORM_W, |v| Message::Ai(AiMsg::ProviderName(v)), Message::Ai(AiMsg::ProviderSave))
    };
    let url_control = if ro {
        value_text(&base_url)
    } else {
        controls::form_input("https://api.example.com/v1", &base_url, FORM_W, |v| {
            Message::Ai(AiMsg::ProviderUrl(v))
        }, Message::Ai(AiMsg::ProviderSave))
    };

    let mut model_col = column![].spacing(4);
    for (i, m) in models.iter().enumerate() {
        let model_control = if ro {
            value_text(m)
        } else {
            controls::form_input("模型 id（如 gpt-4o-mini）", m, 200.0, move |v| {
                Message::Ai(AiMsg::ProviderModel(i, v))
            }, Message::Ai(AiMsg::ProviderSave))
        };
        model_col = model_col.push(
            row![
                model_control,
                controls::icon_button(
                    "删除该模型",
                    12.0,
                    20.0,
                    controls::Variant::Ghost,
                    (!ro).then(|| Message::Ai(AiMsg::ProviderModelRemove(i))),
                    None,
                ),
            ]
            .spacing(4)
            .align_y(iced::alignment::Vertical::Center),
        );
    }
    model_col = model_col.push(controls::btn(
        "添加模型",
        &controls::btn_sm(),
        controls::Variant::Ghost,
        (!ro).then(|| Message::Ai(AiMsg::ProviderModelAdd)),
    ));

    let key_control = if ro {
        value_text(if key.is_empty() && p.api_key.is_none() { "—" } else { "已配置" })
    } else {
        let hint = if sync { "明文随仓库保存" } else { "只存本机密钥链；留空 = 不更改已存" };
        container(
            column![
                controls::secret_input("API Key", &key, FORM_W, |v| Message::Ai(AiMsg::ProviderKey(v)), Message::Ai(AiMsg::ProviderSave)),
                text(hint.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
            ]
            .spacing(2),
        )
        .into()
    };

    let mut col = column![].spacing(8);
    col = col.push(super::group_head("供应商", &p.id, true));
    col = col.push(super::setting_row("名称", "列表与下拉中的显示名", name_control));
    col = col.push(super::setting_row("baseUrl", "OpenAI 兼容接口地址", url_control));
    col = col.push(super::setting_row("模型清单", "对话可选用的模型", model_col.into()));
    col = col.push(super::setting_row("API Key", "请求鉴权凭据", key_control));
    col = col.push(super::setting_row(
        "Key 随仓库同步",
        "开启后 key 明文写入仓库配置（多设备同步有泄露风险）；关闭则只存本机密钥链",
        controls::toggle(sync, (!ro).then(|| Message::Ai(AiMsg::ProviderSyncToggle))),
    ));
    col = col.push(super::setting_row(
        "测试连通性",
        "GET {baseUrl}/models 校验地址与凭据",
        controls::btn(
            if app.ai.testing { "测试中…" } else { "测试连通性" },
            &controls::btn_sm(),
            controls::Variant::Secondary,
            (!ro && !app.ai.testing).then(|| Message::Ai(AiMsg::ProviderTest)),
        )
        .into(),
    ));
    let mut actions = row![].spacing(8);
    actions = actions.push(controls::btn(
        "保存",
        &controls::btn_sm(),
        controls::Variant::Secondary,
        (!ro).then(|| Message::Ai(AiMsg::ProviderSave)),
    ));
    if !ro {
        actions = actions.push(controls::btn(
            "删除",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(open_popup(app, window, PopupState::AiProviderDeleteMenu)),
        ));
    }
    col = col.push(container(actions).width(Length::Fill));
    col.into()
}

// ===== 模型服务 =====

fn models_view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let model_count = app.repo_config.providers.iter().flatten().map(|p| p.models.len()).sum::<usize>();
    let mut col = column![].spacing(8);
    col = col.push(super::group_head("默认模型", "", true));
    let default_trigger = controls::dropdown_trigger(
        &default_model_label(app),
        FORM_W,
        (!ro && model_count > 0).then(|| open_popup(app, window, PopupState::AiModelMenu { for_naming: false })),
    );
    col = col.push(super::setting_row("默认模型", "对话与话题命名缺省使用的模型", default_trigger));
    if model_count == 0 {
        col = col.push(container(
            text("先在「供应商」添加模型").size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ));
    }

    col = col.push(super::group_head("话题自动命名", "", true));
    let mode = naming_mode(app);
    let mode_trigger = controls::dropdown_trigger(
        naming_label(mode),
        FORM_W,
        (!ro).then(|| open_popup(app, window, PopupState::AiNamingMenu)),
    );
    col = col.push(super::setting_row("自动命名", "新话题首条消息后自动起标题", mode_trigger));
    if mode == NamingMode::Custom {
        let model_trigger = controls::dropdown_trigger(
            &naming_model_label(app),
            FORM_W,
            (!ro && model_count > 0).then(|| open_popup(app, window, PopupState::AiModelMenu { for_naming: true })),
        );
        col = col.push(super::setting_row("命名模型", "话题命名一般用小模型；跟随默认 = 用默认模型", model_trigger));
    }
    col.into()
}

fn provider_name(app: &App, id: &str) -> String {
    app.repo_config
        .providers
        .iter()
        .flatten()
        .find(|p| p.id == id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| id.to_string())
}

fn model_label(app: &App, model: Option<&String>, provider_id: Option<&String>) -> String {
    match (model, provider_id) {
        (Some(m), Some(pid)) => format!("{} / {}", provider_name(app, pid), m),
        (Some(m), None) => m.clone(),
        _ => "不指定".into(),
    }
}

fn default_model_label(app: &App) -> String {
    model_label(app, app.repo_config.model.as_ref(), app.repo_config.model_provider_id.as_ref())
}

fn naming_label(mode: NamingMode) -> &'static str {
    match mode {
        NamingMode::Off => "不启用",
        NamingMode::FollowDefault => "跟随默认",
        NamingMode::Custom => "指定模型",
    }
}

fn naming_model_label(app: &App) -> String {
    match &app.repo_config.auto_naming_model {
        Some(o) => format!("{} / {}", provider_name(app, &o.provider_id), o.model),
        None => "跟随默认".into(),
    }
}

// ===== Agent =====

fn agents_view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let mut list = column![].spacing(4);
    for a in &app.agents {
        list = list.push(agent_card(a, app.ai.agent_sel.as_deref() == Some(a.id.as_str())));
    }
    if app.agents.is_empty() {
        list = list.push(text("暂无 Agent，点「新建」开始").size(pal::fs_caption()).color(pal::TEXT_MUTED));
    }
    let new_btn = controls::btn(
        "新建",
        &controls::btn_sm(),
        controls::Variant::Secondary,
        (!ro).then(|| Message::Ai(AiMsg::AgentNew)),
    );

    let right: Element<'static, Message> = match app
        .ai
        .agent_sel
        .as_ref()
        .and_then(|id| app.agents.iter().find(|a| &a.id == id))
    {
        None => container(text("选择或新建 Agent").size(pal::fs_caption()).color(pal::TEXT_MUTED))
            .width(Length::Fill)
            .padding(Padding::ZERO.top(24.0))
            .into(),
        Some(a) => agent_form(app, a, window),
    };

    row![
        container(column![list, space().height(Length::Fixed(8.0)), new_btn].width(Length::Fill))
            .width(Length::Fixed(LIST_W)),
        container(right).width(Length::Fill).padding(Padding::ZERO.left(20.0)),
    ]
    .into()
}

fn agent_card(a: &vault::AgentConfig, selected: bool) -> Element<'static, Message> {
    let mut line = row![].spacing(6);
    line = line.push(
        text(a.name.clone())
            .size(pal::fs_caption())
            .color(if selected { pal::ACCENT } else { pal::TEXT_PRIMARY }),
    );
    if a.builtin.unwrap_or(false) {
        line = line.push(controls::badge("预置", pal::BG_TERTIARY, pal::TEXT_SECONDARY));
    }
    button(line)
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
        .style(controls::row_style(selected, pal::RADIUS_SM))
        .on_press(Message::Ai(AiMsg::AgentSelect(a.id.clone())))
        .into()
}

fn agent_form(app: &App, a: &vault::AgentConfig, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let builtin = a.builtin.unwrap_or(false);
    let draft = app.ai.agent_draft.as_ref().filter(|d| d.id == a.id);
    let name = draft.map(|d| d.name.clone()).unwrap_or_else(|| a.name.clone());
    let prompt = draft.and_then(|d| d.prompt_file.clone()).or_else(|| a.system_prompt_file.clone());
    let tools = draft.map(|d| d.tools.clone()).unwrap_or_else(|| a.tools.clone());

    let name_control = if ro {
        value_text(&name)
    } else {
        controls::form_input("名称", &name, FORM_W, |v| Message::Ai(AiMsg::AgentName(v)), Message::Ai(AiMsg::AgentSave))
    };
    let prompt_label = prompt
        .as_ref()
        .map(|rel| {
            rel.rsplit('/').next().unwrap_or(rel).trim_end_matches(".md").to_string()
        })
        .unwrap_or_else(|| "无".into());
    let prompt_control = if ro {
        value_text(&prompt_label)
    } else {
        controls::dropdown_trigger(&prompt_label, FORM_W, Some(open_popup(app, window, PopupState::AiAgentPromptMenu)))
    };

    let mut col = column![].spacing(8);
    col = col.push(super::group_head("Agent", &a.id, true));
    col = col.push(super::setting_row("名称", "对话入口处的显示名", name_control));
    col = col.push(super::setting_row(
        "系统提示词",
        "引用提示词标记的笔记，发送时注入正文",
        prompt_control,
    ));

    col = col.push(super::group_head("工具", "勾选后该 Agent 的对话可调用", true));
    for (ci, cat) in TOOL_CATEGORIES.iter().enumerate() {
        let state = category_state(&tools, cat);
        let head = row![
            controls::checkbox(state, (!ro).then(|| Message::Ai(AiMsg::AgentCategoryToggle(ci)))),
            text(cat.label.to_string())
                .size(pal::fs_caption())
                .color(pal::TEXT_PRIMARY)
                .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);
        col = col.push(container(head).padding(Padding::ZERO.horizontal(8.0).top(4.0)));
        for t in cat.tools {
            let on = tools.iter().any(|x| x == t);
            let tool_row = row![
                controls::checkbox(
                    if on { controls::CheckState::On } else { controls::CheckState::Off },
                    (!ro).then(|| Message::Ai(AiMsg::AgentToolToggle(t.to_string()))),
                ),
                text(t.to_string()).size(pal::fs_caption()).color(pal::TEXT_SECONDARY).font(mono()),
            ]
            .spacing(6)
            .align_y(iced::alignment::Vertical::Center);
            col = col.push(container(tool_row).padding(Padding::ZERO.left(20.0)));
        }
    }

    let mut actions = row![].spacing(8);
    actions = actions.push(controls::btn(
        "保存",
        &controls::btn_sm(),
        controls::Variant::Secondary,
        (!ro).then(|| Message::Ai(AiMsg::AgentSave)),
    ));
    if !builtin && !ro {
        actions = actions.push(controls::btn(
            "删除",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(open_popup(app, window, PopupState::AiAgentDeleteMenu)),
        ));
    }
    col = col.push(container(actions).width(Length::Fill));
    if builtin {
        col = col.push(container(
            text("预置 Agent 不可删除").size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ));
    }
    col.into()
}

fn category_state(tools: &[String], cat: &ToolCategory) -> controls::CheckState {
    let hit = cat.tools.iter().filter(|t| tools.iter().any(|x| x == *t)).count();
    if hit == 0 {
        controls::CheckState::Off
    } else if hit == cat.tools.len() {
        controls::CheckState::On
    } else {
        controls::CheckState::Partial
    }
}

// ===== 联网搜索 =====

fn search_view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let sync = app.repo_config.sync_keys.unwrap_or(false);
    let current_search = app.repo_config.search.clone().unwrap_or_default();
    let draft = app.ai.search_draft.clone();
    let provider = draft
        .as_ref()
        .map(|d| d.provider.clone())
        .unwrap_or_else(|| current_search.provider.clone());
    let searxng_url = draft
        .as_ref()
        .map(|d| d.searxng_url.clone())
        .unwrap_or_else(|| current_search.searxng_url.clone());
    let provider_label = match provider.as_str() {
        "searxng" => "SearXNG",
        // 空值 = 未配置，缺省语义按 Tavily 展示
        _ => "Tavily",
    };

    let provider_control = if ro {
        value_text(provider_label)
    } else {
        controls::dropdown_trigger(provider_label, FORM_W, Some(open_popup(app, window, PopupState::AiSearchProviderMenu)))
    };

    let mut col = column![].spacing(8);
    col = col.push(super::group_head("搜索源", "", true));
    col = col.push(super::setting_row("搜索提供方", "AI 联网搜索工具使用的后端", provider_control));
    if provider == "searxng" {
        let url_control = if ro {
            value_text(&searxng_url)
        } else {
            controls::form_input("http://127.0.0.1:8080", &searxng_url, FORM_W, |v| {
                Message::Ai(AiMsg::SearchSearxUrl(v))
            }, Message::Ai(AiMsg::SearchSave))
        };
        col = col.push(super::setting_row(
            "SearXNG 实例 URL",
            "自建实例地址（需在实例 settings.yml 的 search.formats 加入 json）",
            url_control,
        ));
    } else {
        let key_control = if ro {
            value_text(if sync && current_search.tavily_api_key.is_some() { "已配置" } else { "—" })
        } else {
            let hint = if sync { "明文随仓库保存" } else { "只存本机密钥链；留空 = 不更改已存" };
            container(
                column![
                    controls::secret_input(
                        "Tavily API Key",
                        draft.as_ref().map(|d| d.tavily_key.as_str()).unwrap_or(""),
                        FORM_W,
                        |v| Message::Ai(AiMsg::SearchTavilyKey(v)),
                        Message::Ai(AiMsg::SearchSave),
                    ),
                    text(hint.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                ]
                .spacing(2),
            )
            .into()
        };
        col = col.push(super::setting_row("Tavily API Key", "请求鉴权凭据", key_control));
    }
    col = col.push(container(
        controls::btn(
            "保存",
            &controls::btn_sm(),
            controls::Variant::Secondary,
            (!ro).then(|| Message::Ai(AiMsg::SearchSave)),
        )
        .width(Length::Fixed(60.0)),
    ));
    col.into()
}
