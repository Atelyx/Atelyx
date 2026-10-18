//! 设置-AI：供应商 / 模型服务 / Agent / 联网搜索（仓库级，本地落 `.atelyx/`，空间落服务端）。
//! 编辑走草稿 + 保存；API key 缺省只进密钥链，`syncKeys` 开启才随仓库落盘（切换落点先迁移不丢 key）。

use iced::widget::{button, column, container, row, scrollable, space};
use iced::{Element, Length, Padding, Task, border, widget::container::Style};

use atelyx_core::credentials;
use atelyx_core::vault;

use crate::layout_view::PopupState;
use crate::space_actions;
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::ui::text;
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

/// 工具分组（id 与 AI 工具注册名一致；空清单 = 纯对话）。
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
];

/// 插件工具在 `TOOL_CATEGORIES` 之后单列一组（动态清单，不占静态表）。
fn plugin_cat_index() -> usize {
    TOOL_CATEGORIES.len()
}

/// 一类工具的勾选清单（id + 显示名）：静态分类取名册，插件组取当前注册工具的 selection id。
fn category_items(app: &App, index: usize) -> Vec<(String, String)> {
    if index == plugin_cat_index() {
        app.plugins
            .plugin_tools()
            .into_iter()
            .map(|t| {
                let label = format!("{} · {}", t.plugin_name, t.name);
                (t.selection_id(), label)
            })
            .collect()
    } else {
        TOOL_CATEGORIES
            .get(index)
            .map(|cat| {
                cat.tools
                    .iter()
                    .map(|t| (t.to_string(), tool_label(t)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 工具显示名（对话面板工具行 / 设置页工具清单共用）。未知 id 原样返回。
pub(crate) fn tool_label(id: &str) -> String {
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

/// 选中项兜底：打开 AI 页 / 切子分区 / 清单变化时，未选中就落到首位——
/// 右侧直接呈现首项表单，不留「选择或添加」的占位空态（清单为空时另有空态）。
pub fn ensure_selection(app: &mut App) {
    let providers = app.repo_config.providers.clone().unwrap_or_default();
    let provider_gone = app
        .ai
        .provider_sel
        .as_ref()
        .is_none_or(|id| !providers.iter().any(|p| &p.id == id));
    if provider_gone {
        app.ai.provider_sel = providers.first().map(|p| p.id.clone());
        app.ai.provider_draft = providers.first().map(|p| provider_draft_of(app, p));
    }
    let agent_gone = app
        .ai
        .agent_sel
        .as_ref()
        .is_none_or(|id| !app.agents.iter().any(|a| &a.id == id));
    if agent_gone {
        app.ai.agent_sel = app.agents.first().map(|a| a.id.clone());
        app.ai.agent_draft = app.agents.first().map(agent_draft_of);
    }
    if app.ai.collapsed_cats.len() != TOOL_CATEGORIES.len() + 1 {
        app.ai.collapsed_cats = vec![true; TOOL_CATEGORIES.len() + 1];
    }
    app.ai.search_ready = search_source_ready(app);
}

/// 搜索源就绪：tavily = 密钥可用（随仓明文或本机密钥链）；searxng = 地址非空。
fn search_source_ready(app: &App) -> bool {
    let Some(search) = &app.repo_config.search else { return false };
    match search.provider.as_str() {
        "tavily" => {
            if search.tavily_api_key.as_deref().is_some_and(|k| !k.is_empty()) {
                return true;
            }
            key_scope(app)
                .and_then(|scope| {
                    crate::credentials::load_provider_key(&scope, "search-tavily").ok().flatten()
                })
                .is_some_and(|k| !k.is_empty())
        }
        "searxng" => !search.searxng_url.is_empty(),
        _ => false,
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
    /// 最近一次连通性测试结果（成功带耗时毫秒 / 失败带原因；切换供应商时清除）。
    pub test_result: Option<Result<u128, String>>,
    /// 模型列表拉取在途（防重入；与测试互斥）。
    pub fetching: bool,
    /// 最近一次拉取到的供应商模型 id（None = 未拉取或失败；切换供应商时清除）。
    pub fetched: Option<Vec<String>>,
    /// 最近一次拉取失败原因（切换供应商时清除）。
    pub fetch_error: Option<String>,
    /// 待清理的密钥链条目 (scope, 供应商 id)：供应商删除在配置落盘成功后执行。
    pub provider_key_delete_pending: Option<(String, String)>,
    /// Agent 列表悬停行（悬停显形复制/删除操作钮）。
    pub agent_hover: Option<String>,
    /// 工具分类折叠状态（缺省全折叠；不持久化，进设置页重置）。
    pub collapsed_cats: Vec<bool>,
    /// 搜索源就绪（未就绪时工具行的联网项给降级提示）。
    pub search_ready: bool,
}

/// 供应商表单草稿（api_key 明文仅存内存；保存按 syncKeys 决定落点）。
#[derive(Clone)]
pub struct ProviderDraft {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// 已选模型（含可选昵称；清单来自服务端拉取勾选或手动添加）。
    pub models: Vec<vault::VaultModel>,
    /// 手动添加模型的输入草稿（回车 / 点添加入列）。
    pub manual_draft: String,
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
    ProviderModelRemove(usize),
    ProviderKey(String),
    ProviderSyncToggle,
    ProviderSave,
    ProviderTest,
    ProviderTestDone(u64, Result<u128, String>),
    /// 拉取供应商模型列表（GET /models；成功回填勾选清单）。
    ProviderFetch,
    ProviderFetchDone(u64, Result<(Vec<String>, u128), String>),
    /// 勾选 / 取消一个服务端模型（按模型 id）。
    ProviderModelToggle(String),
    /// 手动添加模型输入（回车或点「添加」入列）。
    ProviderManualDraft(String),
    ProviderManualAdd,
    ProviderNickname(usize, String),
    ProviderDelete,
    // 模型服务
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
    /// Agent 列表行悬停（悬停显形操作钮）。
    AgentHover(Option<String>),
    /// 复制 Agent（新 id 副本，紧随原条目入列）。
    AgentDuplicate(String),
    /// 工具分类折叠切换。
    AgentCatCollapse(usize),
    /// 提示词失效收敛完成（missing = 已不存在的路径集合）。
    PromptPruneDone(u64, Vec<String>),
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
    let mut col = column![].spacing(8).padding(Padding::ZERO.horizontal(20.0).top(4.0));
    col = col.push(sub_tab_bar(app));
    if readonly(app) {
        col = col.push(read_only_banner());
    }
    // 子分区自持滚动：列表列与表单列各自滚动，中间分隔线随行高撑满
    // （套一层外层滚动会把 Fill 高的两栏压扁，表单只剩首行）
    col = col.push(
        container(match app.ai_sub_tab.as_str() {
            "models" => models_view(app, window),
            "agents" => agents_view(app, window),
            "search" => search_view(app, window),
            _ => providers_view(app, window),
        })
        .width(Length::Fill)
        .height(Length::Fill),
    );
    container(col).width(Length::Fill).height(Length::Fill).into()
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

/// 提示词失效收敛（幂等自愈）：注册列表与 Agent 引用中指向已删除笔记的路径清零。
/// 存在性校验失败（网络抖动等）按存在处理，本轮跳过；写盘失败静默，下轮再收敛。
pub fn prune_missing_prompts(app: &App) -> Task<Message> {
    let mut paths = app.prompt_notes.clone();
    for f in app.agents.iter().filter_map(|a| a.system_prompt_file.clone()) {
        if !paths.contains(&f) {
            paths.push(f);
        }
    }
    if paths.is_empty() {
        return Task::none();
    }
    let gen = app.epoch();
    let probe = match &app.repo_kind {
        crate::RepoKind::Local => {
            let Some(root) = app.vault_root.clone() else { return Task::none() };
            Probe::Local { root, paths }
        }
        crate::RepoKind::Space(session) => Probe::Space {
            client: session.client.clone(),
            space_id: session.space_id.clone(),
            paths,
        },
    };
    Task::perform(
        async move {
            let mut missing: Vec<String> = Vec::new();
            match probe {
                Probe::Local { root, paths } => {
                    for f in paths {
                        // 只认「文件不在」：读权限/占用类失败不误判为已删除
                        if !atelyx_core::vault::note_exists(&root, &f) {
                            missing.push(f);
                        }
                    }
                }
                Probe::Space { client, space_id, paths } => {
                    for f in paths {
                        // NotFound = 已删除；其余错误按存在处理（本轮跳过）
                        let gone = matches!(
                            client.get_file_text(&space_id, &f).await,
                            Err(atelyx_core::space_client::SpaceError::NotFound)
                        );
                        if gone {
                            missing.push(f);
                        }
                    }
                }
            }
            missing
        },
        move |missing| Message::Ai(AiMsg::PromptPruneDone(gen, missing)),
    )
}

/// 存在性探针：把仓库访问凭据从 App 拆出，供异步块独立持有。
enum Probe {
    Local {
        root: std::path::PathBuf,
        paths: Vec<String>,
    },
    Space {
        client: std::sync::Arc<atelyx_core::space_client::SpaceClient>,
        space_id: String,
        paths: Vec<String>,
    },
}

/// agents.json 写盘（本地 = 原子写后重读；空间 = 整表 PUT 后重读；回执经 AgentsLoaded）。
pub fn save_agents_pub(app: &mut App, agents: Vec<vault::AgentConfig>) -> Task<Message> {
    save_agents(app, agents)
}

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
            ensure_selection(app);
            if id == "agents" {
                let prune = prune_missing_prompts(app);
                return Task::batch([load_agents(app), prune]);
            }
            Task::none()
        }
        AiMsg::PromptPruneDone(gen, missing) => {
            if gen != app.epoch() || missing.is_empty() {
                return Task::none();
            }
            let mut tasks: Vec<Task<Message>> = Vec::new();
            // 注册列表收敛（写回执经 PromptNotesLoaded 对齐内存）
            let notes: Vec<String> = app
                .prompt_notes
                .iter()
                .filter(|f| !missing.contains(f))
                .cloned()
                .collect();
            if notes.len() != app.prompt_notes.len() {
                let cleared = app.prompt_notes.len() - notes.len();
                app.prompt_notes = notes.clone();
                let gen = app.epoch();
                match &app.repo_kind {
                    crate::RepoKind::Local => {
                        if let Some(root) = app.vault_root.clone() {
                            tasks.push(Task::perform(
                                async move {
                                    atelyx_core::vault::write_prompt_notes_file(&root, &notes)?;
                                    atelyx_core::vault::read_prompt_notes_file(&root)
                                },
                                move |result| Message::PromptNotesLoaded(gen, result),
                            ));
                        }
                    }
                    crate::RepoKind::Space(session) => {
                        // 只读成员不发起无权限写盘（服务端会拒绝并污染通知）
                        if !session.role.can_write() {
                            app.notify(NoticeLevel::Info, "只读身份：跳过提示词失效清理");
                        } else {
                            let client = session.client.clone();
                            let space_id = session.space_id.clone();
                            let json = serde_json::to_string(&notes).unwrap_or_default();
                            tasks.push(Task::perform(
                                async move {
                                    client
                                        .put_file_text(&space_id, ".atelyx/prompt-notes.json", &json)
                                        .await?;
                                    Ok::<_, atelyx_core::space_client::SpaceError>(notes)
                                },
                                move |result| {
                                    Message::PromptNotesLoaded(gen, result.map_err(|e| e.to_string()))
                                },
                            ));
                        }
                    }
                }
                app.notify(NoticeLevel::Info, format!("已清理 {cleared} 条失效提示词标记"));
            }
            // Agent 引用收敛（与注册列表对称；写失败不回滚注册列表，下轮再收敛残留）
            let mut cleared_refs = 0usize;
            let agents: Vec<vault::AgentConfig> = app
                .agents
                .iter()
                .map(|a| {
                    let mut a = a.clone();
                    if a.system_prompt_file
                        .as_deref()
                        .is_some_and(|f| missing.iter().any(|m| m == f))
                    {
                        a.system_prompt_file = None;
                        cleared_refs += 1;
                    }
                    a
                })
                .collect();
            if cleared_refs > 0 {
                app.agents = agents.clone();
                app.ai.agent_draft = None;
                tasks.push(save_agents(app, agents));
                app.notify(
                    NoticeLevel::Info,
                    format!("移除 {cleared_refs} 个 Agent 的提示词引用"),
                );
            }
            if tasks.is_empty() {
                Task::none()
            } else {
                Task::batch(tasks)
            }
        }

        // ---- 供应商 ----
        AiMsg::ProviderSelect(id) => {
            app.ai.provider_sel = Some(id.clone());
            app.ai.test_result = None;
            // 拉取结果按供应商区分：切换即清空（下个供应商要重新拉取）
            app.ai.fetched = None;
            app.ai.fetch_error = None;
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
            let Some(p) = PROVIDER_PRESETS.get(i) else { return Task::none() };
            let models = p.models.iter().map(|m| vault::VaultModel { id: m.to_string(), nickname: None }).collect();
            add_provider(app, p.name, p.base_url, models)
        }
        AiMsg::ProviderAddCustom => add_provider(app, "自定义", "", Vec::new()),
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
        AiMsg::ProviderModelToggle(id) => {
            let Some(d) = app.ai.provider_draft.as_mut() else { return Task::none() };
            if let Some(pos) = d.models.iter().position(|m| m.id == id) {
                d.models.remove(pos);
            } else {
                d.models.push(vault::VaultModel { id, nickname: None });
            }
            update(app, AiMsg::ProviderSave)
        }
        AiMsg::ProviderManualDraft(v) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.manual_draft = v;
            }
            Task::none()
        }
        AiMsg::ProviderManualAdd => {
            let Some(d) = app.ai.provider_draft.as_mut() else { return Task::none() };
            let id = d.manual_draft.trim().to_string();
            d.manual_draft.clear();
            if id.is_empty() || d.models.iter().any(|m| m.id == id) {
                return Task::none();
            }
            d.models.push(vault::VaultModel { id, nickname: None });
            update(app, AiMsg::ProviderSave)
        }
        AiMsg::ProviderNickname(i, v) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                if let Some(m) = d.models.get_mut(i) {
                    let v = v.trim().to_string();
                    m.nickname = (!v.is_empty()).then_some(v);
                }
            }
            Task::none()
        }
        AiMsg::ProviderModelRemove(i) => {
            if let Some(d) = app.ai.provider_draft.as_mut() {
                d.models.remove(i);
            }
            update(app, AiMsg::ProviderSave)
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
            update(app, AiMsg::ProviderSave)
        }
        AiMsg::ProviderSave => {
            let Some(draft) = app.ai.provider_draft.clone() else { return Task::none() };
            // 保存后旧地址的连通性结果不再适用，一并清除
            app.ai.test_result = None;
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
            app.ai.test_result = None;
            let gen = app.epoch();
            Task::perform(
                async move { atelyx_core::ai::provider::test_endpoint(&base_url, &key).await },
                move |result| Message::Ai(AiMsg::ProviderTestDone(gen, result)),
            )
        }
        AiMsg::ProviderTestDone(gen, result) => {
            if gen != app.epoch() {
                return Task::none();
            }
            app.ai.testing = false;
            match &result {
                Ok(ms) => app.notify(NoticeLevel::Success, format!("连接成功（{ms}ms）")),
                Err(e) => app.notify(NoticeLevel::Error, format!("连接失败：{e}")),
            }
            app.ai.test_result = Some(result);
            Task::none()
        }
        AiMsg::ProviderFetch => {
            if app.ai.fetching || readonly(app) {
                return Task::none();
            }
            let (base_url, key) = provider_test_inputs(app);
            if base_url.trim().is_empty() {
                app.notify(NoticeLevel::Warning, "先填写 baseUrl");
                return Task::none();
            }
            app.ai.fetching = true;
            app.ai.fetch_error = None;
            let gen = app.epoch();
            Task::perform(
                async move { atelyx_core::ai::provider::list_models(&base_url, &key).await },
                move |result| Message::Ai(AiMsg::ProviderFetchDone(gen, result)),
            )
        }
        AiMsg::ProviderFetchDone(gen, result) => {
            if gen != app.epoch() {
                // 容器已切换：拉取结果不回投，但必须解除在途标记（新容器重建 AiState 时才会归零）
                app.ai.fetching = false;
                return Task::none();
            }
            app.ai.fetching = false;
            match result {
                Ok((ids, ms)) => {
                    app.ai.fetched = Some(ids);
                    app.ai.test_result = Some(Ok(ms));
                }
                Err(e) => {
                    app.ai.fetched = None;
                    app.ai.fetch_error = Some(e);
                }
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
            update(app, AiMsg::AgentSave)
        }
        AiMsg::AgentToolToggle(tool) => {
            if let Some(d) = app.ai.agent_draft.as_mut() {
                if let Some(i) = d.tools.iter().position(|t| t == &tool) {
                    d.tools.remove(i);
                } else {
                    d.tools.push(tool);
                }
            }
            update(app, AiMsg::AgentSave)
        }
        AiMsg::AgentCategoryToggle(i) => {
            let items = category_items(app, i);
            if items.is_empty() {
                return Task::none();
            }
            if let Some(d) = app.ai.agent_draft.as_mut() {
                let all_on = items.iter().all(|(t, _)| d.tools.iter().any(|x| x == t));
                if all_on {
                    d.tools.retain(|x| !items.iter().any(|(t, _)| t == x));
                } else {
                    for (t, _) in &items {
                        if !d.tools.iter().any(|x| x == t) {
                            d.tools.push(t.clone());
                        }
                    }
                }
            }
            update(app, AiMsg::AgentSave)
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
        AiMsg::AgentHover(id) => {
            app.ai.agent_hover = id;
            Task::none()
        }
        AiMsg::AgentDuplicate(id) => {
            app.dismiss_popup();
            if readonly(app) {
                return Task::none();
            }
            let Some(src) = app.agents.iter().find(|a| a.id == id).cloned() else {
                return Task::none();
            };
            let mut agents = app.agents.clone();
            if let Some(pos) = agents.iter().position(|a| a.id == id) {
                let copy = vault::AgentConfig {
                    id: format!("agent-{}", nanoid::nanoid!(8)),
                    name: format!("{} 副本", src.name),
                    system_prompt_file: src.system_prompt_file.clone(),
                    tools: src.tools.clone(),
                    builtin: None,
                };
                agents.insert(pos + 1, copy);
            }
            save_agents(app, agents)
        }
        AiMsg::AgentCatCollapse(i) => {
            if let Some(v) = app.ai.collapsed_cats.get_mut(i) {
                *v = !*v;
            }
            Task::none()
        }
        AiMsg::AgentDelete => {
            app.dismiss_popup();
            if readonly(app) {
                return Task::none();
            }
            let Some(id) = app.ai.agent_sel.clone() else { return Task::none() };
            // 内置 Agent 兜底保护（入口按钮已隐藏，防其它路径误删）：守卫先行，不动选中态
            if app
                .agents
                .iter()
                .any(|a| a.id == id && a.builtin.unwrap_or(false))
            {
                return Task::none();
            }
            app.ai.agent_sel = None;
            app.ai.agent_draft = None;
            let mut agents = app.agents.clone();
            agents.retain(|a| a.id != id);
            app.agents = agents.clone();
            save_agents(app, agents)
        }

        // ---- 联网搜索 ----
        AiMsg::SearchProviderPick(v) => {
            ensure_search_draft(app).provider = v;
            update(app, AiMsg::SearchSave)
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
        models: p.models.clone(),
        manual_draft: String::new(),
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
fn add_provider(app: &mut App, name: &str, base_url: &str, models: Vec<vault::VaultModel>) -> Task<Message> {
    if readonly(app) {
        return Task::none();
    }
    let id = format!("prov-{}", nanoid::nanoid!(8));
    let draft_models = models.clone();
    let mut providers = app.repo_config.providers.clone().unwrap_or_default();
    providers.push(vault::VaultProvider {
        id: id.clone(),
        name: name.to_string(),
        base_url: base_url.to_string(),
        models,
        api_key: None,
    });
    let Ok(list) = serde_json::to_value(&providers) else {
        app.notify(NoticeLevel::Error, "供应商配置序列化失败");
        return Task::none();
    };
    app.repo_config.providers = Some(providers);
    app.ai.provider_sel = Some(id.clone());
    app.ai.fetched = None;
    app.ai.fetch_error = None;
    // 草稿与入库条目同基线：否则后续任一次保存会用空清单覆盖预设模型
    app.ai.provider_draft = Some(ProviderDraft {
        id,
        name: name.to_string(),
        base_url: base_url.to_string(),
        models: draft_models,
        manual_draft: String::new(),
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
    let models: Vec<vault::VaultModel> = draft
        .models
        .iter()
        .map(|m| vault::VaultModel {
            id: m.id.trim().to_string(),
            nickname: m.nickname.clone().filter(|n| !n.trim().is_empty()),
        })
        .filter(|m| !m.id.is_empty())
        .collect();
    let sync = draft.sync_keys;
    let entry = &mut providers[index];
    entry.name = draft.name.trim().to_string();
    entry.base_url = draft.base_url.trim().to_string();
    entry.models = models;
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

    // 左列：供应商卡片 + 底部「快速添加」（预设与自定义直接入列）
    let mut list = column![].spacing(6);
    for p in &providers {
        list = list.push(provider_card(p, app.ai.provider_sel.as_deref() == Some(p.id.as_str())));
    }
    if providers.is_empty() {
        list = list.push(
            text(if is_space(app) { "该空间还没有配置模型供应商" } else { "还没有供应商，从下方添加" })
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
    }
    let mut quick = column![].spacing(2);
    quick = quick.push(
        text("快速添加".to_string())
            .size(pal::fs_micro())
            .color(pal::TEXT_MUTED),
    );
    for (i, p) in PROVIDER_PRESETS.iter().enumerate() {
        quick = quick.push(quick_add_row(p.name, (!ro).then(|| Message::Ai(AiMsg::ProviderAddPreset(i)))));
    }
    quick = quick.push(quick_add_row("自定义", (!ro).then(|| Message::Ai(AiMsg::ProviderAddCustom))));

    let left = container(
        scrollable(
            container(
                column![
                    list,
                    space().height(Length::Fixed(12.0)),
                    controls::h_divider_subtle(),
                    container(quick)
                        .width(Length::Fill)
                        .padding(Padding::ZERO.left(4.0).top(10.0)),
                ]
                .width(Length::Fill),
            )
            .width(Length::Fill)
            .padding(Padding::ZERO.right(4.0).bottom(12.0)),
        )
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fixed(LIST_W))
    .height(Length::Fill);

    let right: Element<'static, Message> = match app
        .ai
        .provider_sel
        .as_ref()
        .and_then(|id| providers.iter().find(|p| &p.id == id))
    {
        // 空态要在整列里居中，不套滚动
        None => container(controls::empty_state(
            "bot",
            "还没有配置供应商",
            "从左侧快速添加一个预设（OpenAI / DeepSeek / 通义千问）或自定义，填好地址与 API Key 后即可对话",
            (!ro).then(|| (
                "添加自定义供应商".to_string(),
                Message::Ai(AiMsg::ProviderAddCustom),
            )),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        Some(p) => scrollable(
            container(provider_form(app, p, window)).padding(Padding::ZERO.left(20.0).bottom(24.0)),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    };

    let body = row![
        left,
        list_divider(),
        container(right).width(Length::Fill).height(Length::Fill),
    ]
    .height(Length::Fill);

    // 底栏：本地仓库 = key 落盘策略开关；空间 = 团队共享说明（key 由服务端承载，无开关）
    let bottom: Element<'static, Message> = if is_space(app) {
        container(
            text("协作空间下供应商配置（含 API Key）保存在服务端、空间成员共用一份".to_string())
                .size(pal::fs_caption())
                .color(if ro { pal::WARNING } else { pal::TEXT_MUTED }),
        )
        .width(Length::Fill)
        .into()
    } else {
        let sync = app
            .ai
            .provider_draft
            .as_ref()
            .map(|d| d.sync_keys)
            .unwrap_or_else(|| app.repo_config.sync_keys.unwrap_or(false));
        row![
            column![
                text("API key 随仓库保存".to_string()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
                text("开启后 key 明文写入仓库配置（多设备同步有泄露风险）；关闭则只存本机密钥链".to_string())
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            ]
            .spacing(2)
            .width(Length::Fill),
            controls::toggle(sync, (!ro && app.ai.provider_draft.is_some()).then(|| Message::Ai(AiMsg::ProviderSyncToggle))),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center)
        .into()
    };

    column![
        body,
        controls::h_divider_subtle(),
        container(bottom).width(Length::Fill).padding(Padding::new(20.0).vertical(8.0)),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn is_space(app: &App) -> bool {
    matches!(&app.repo_kind, crate::RepoKind::Space(_))
}

/// 快速添加行（+ 图标 + 名称；整行可点）。
fn quick_add_row(label: &str, on_press: Option<Message>) -> Element<'static, Message> {
    button(
        row![
            icon("plus", 11.0, pal::TEXT_SECONDARY),
            text(label.to_string()).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::new(8.0).vertical(5.0))
    .style(controls::row_style(false, pal::RADIUS_SM))
    .on_press_maybe(on_press)
    .into()
}

/// 列表列与表单列之间的 1px 分隔（两个功能区）。
fn list_divider() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_t| Style {
            background: Some(pal::BORDER_SUBTLE.into()),
            ..Style::default()
        })
        .into()
}

/// 供应商卡片：名称 + 模型数徽标 + 去 协议 主机名（选中 = 强调描边 + 柔底）。
fn provider_card(p: &vault::VaultProvider, selected: bool) -> Element<'static, Message> {
    let host = p
        .base_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = if host.is_empty() { "未设置地址" } else { host };
    button(
        column![
            row![
                text(p.name.clone())
                    .size(pal::fs_caption())
                    .color(if selected { pal::ACCENT } else { pal::TEXT_PRIMARY })
                    .width(Length::Fill),
                controls::badge(format!("{} 模型", p.models.len()), pal::BG_TERTIARY, pal::TEXT_MUTED),
            ]
            .spacing(4)
            .align_y(iced::alignment::Vertical::Center),
            text(host.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ]
        .spacing(3),
    )
    .width(Length::Fill)
    .padding(Padding::new(10.0).vertical(7.0))
    .style(move |_t, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(
                if selected {
                    pal::ACCENT_SOFT
                } else if hovered {
                    pal::HOVER
                } else {
                    pal::BG_TERTIARY
                }
                .into(),
            ),
            text_color: pal::TEXT_PRIMARY,
            border: border::Border {
                width: if selected { 1.0 } else { 0.5 },
                radius: pal::RADIUS_MD.into(),
                color: if selected { pal::ACCENT } else { pal::BORDER },
            },
            ..Default::default()
        }
    })
    .on_press(Message::Ai(AiMsg::ProviderSelect(p.id.clone())))
    .into()
}

/// 表单字段（标签在上、控件在下；旧壳 Field 布局）。
fn field(label: impl Into<String>, control: Element<'static, Message>) -> Element<'static, Message> {
    column![
        text(label.into()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
        container(control).width(Length::Fill).padding(Padding::ZERO.top(2.0)),
    ]
    .spacing(0)
    .into()
}

fn provider_form(app: &App, p: &vault::VaultProvider, window: iced::window::Id) -> Element<'static, Message> {
    let _ = window;
    let ro = readonly(app);
    let draft = app.ai.provider_draft.as_ref().filter(|d| d.id == p.id);
    let name = draft.map(|d| d.name.clone()).unwrap_or_else(|| p.name.clone());
    let base_url = draft.map(|d| d.base_url.clone()).unwrap_or_else(|| p.base_url.clone());
    let models = draft.map(|d| d.models.clone()).unwrap_or_else(|| p.models.clone());
    let key = draft.map(|d| d.api_key.clone()).unwrap_or_default();
    let sync = draft.map(|d| d.sync_keys).unwrap_or_else(|| app.repo_config.sync_keys.unwrap_or(false));

    let name_control = if ro {
        value_text(&name)
    } else {
        controls::form_input("My Provider", &name, FORM_W, |v| Message::Ai(AiMsg::ProviderName(v)), Message::Ai(AiMsg::ProviderSave))
    };
    let url_control = if ro {
        value_text(&base_url)
    } else {
        controls::form_input("https://api.openai.com/v1", &base_url, FORM_W, |v| {
            Message::Ai(AiMsg::ProviderUrl(v))
        }, Message::Ai(AiMsg::ProviderSave))
    };

    // API Key 行：密文输入 + 测试连通性按钮（结果行内反馈，不弹通知）
    let key_control = if ro {
        value_text(if key.is_empty() && p.api_key.is_none() { "—" } else { "已配置" })
    } else {
        let hint = if sync { "明文随仓库保存" } else { "只存本机密钥链；留空 = 不更改已存" };
        let test_btn = controls::btn(
            if app.ai.testing { "测试中…" } else { "测试连通性" },
            &controls::btn_sm(),
            controls::Variant::Secondary,
            (!ro && !app.ai.testing).then(|| Message::Ai(AiMsg::ProviderTest)),
        );
        let mut key_col = column![
            row![controls::secret_input("sk-...", &key, FORM_W, |v| Message::Ai(AiMsg::ProviderKey(v)), Message::Ai(AiMsg::ProviderSave)), test_btn]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
            text(hint.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ]
        .spacing(4);
        match &app.ai.test_result {
            Some(Ok(ms)) => {
                key_col = key_col.push(
                    row![
                        icon("check", 12.0, pal::SUCCESS),
                        text(format!("连接成功 · {ms}ms")).size(pal::fs_micro()).color(pal::SUCCESS),
                    ]
                    .spacing(4),
                );
            }
            Some(Err(e)) => {
                key_col = key_col.push(
                    row![
                        icon("x", 12.0, pal::DANGER),
                        text(format!("连接失败 · {e}")).size(pal::fs_micro()).color(pal::DANGER),
                    ]
                    .spacing(4),
                );
            }
            None => {}
        }
        key_col.into()
    };

    // 模型区：拉取按钮 + 服务端清单勾选 ∪ 手动模型（徽标标示）+ 昵称 + 手动添加
    let fetched = app.ai.fetched.clone();
    let fetch_btn = controls::btn(
        if app.ai.fetching { "获取中…" } else { "获取模型列表" },
        &controls::btn_sm(),
        controls::Variant::Secondary,
        (!ro && !app.ai.fetching).then(|| Message::Ai(AiMsg::ProviderFetch)),
    );
    let mut model_area = column![].spacing(4);
    model_area = model_area.push(
        row![fetch_btn, text("勾选要使用的模型，可设置昵称".to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED)]
            .spacing(10)
            .align_y(iced::alignment::Vertical::Center),
    );
    if let Some(e) = &app.ai.fetch_error {
        model_area = model_area.push(
            row![
                icon("x", 12.0, pal::DANGER),
                text(format!("获取失败：{e}")).size(pal::fs_micro()).color(pal::DANGER),
            ]
            .spacing(4),
        );
    }
    // 行 = 服务端模型（可勾选）∪ 已添加但不在服务端清单的手动模型（固定选中）
    let mut rows: Vec<(String, bool)> = fetched
        .unwrap_or_default()
        .into_iter()
        .map(|id| (id, true))
        .collect();
    for m in &models {
        if !rows.iter().any(|(id, _)| id == &m.id) {
            rows.push((m.id.clone(), false));
        }
    }
    if rows.is_empty() && app.ai.fetch_error.is_none() {
        model_area = model_area.push(
            text("点击「获取模型列表」拉取，或在下方手动添加模型 ID".to_string())
                .size(pal::fs_micro())
                .color(pal::TEXT_MUTED),
        );
    }
    for (id, from_server) in &rows {
        let sel = models.iter().position(|m| &m.id == id);
        let mut row_ctrl = row![].spacing(8).align_y(iced::alignment::Vertical::Center);
        if *from_server {
            row_ctrl = row_ctrl.push(controls::checkbox(
                if sel.is_some() { controls::CheckState::On } else { controls::CheckState::Off },
                (!ro).then(|| Message::Ai(AiMsg::ProviderModelToggle(id.clone()))),
            ));
        } else {
            row_ctrl = row_ctrl.push(controls::badge("手动", pal::BG_TERTIARY, pal::TEXT_MUTED));
        }
        row_ctrl = row_ctrl.push(
            text(id.clone())
                .size(pal::fs_caption())
                .color(pal::TEXT_PRIMARY)
                .width(Length::Fill),
        );
        if let Some(pos) = sel {
            if !ro {
                let nickname = models[pos].nickname.clone().unwrap_or_default();
                row_ctrl = row_ctrl.push(controls::form_input(
                    "昵称（可选）",
                    &nickname,
                    140.0,
                    move |v| Message::Ai(AiMsg::ProviderNickname(pos, v)),
                    Message::Ai(AiMsg::ProviderSave),
                ));
            }
            row_ctrl = row_ctrl.push(controls::icon_button(
                "x",
                12.0,
                20.0,
                controls::Variant::Ghost,
                (!ro).then(|| Message::Ai(AiMsg::ProviderModelRemove(pos))),
                None,
            ));
        }
        model_area = model_area.push(
            container(row_ctrl)
                .width(Length::Fill)
                .padding(Padding::new(8.0).vertical(4.0))
                .style(|_t| Style {
                    background: Some(pal::BG_SECONDARY.into()),
                    border: border::rounded(pal::RADIUS_SM),
                    ..Style::default()
                }),
        );
    }
    if !ro {
        model_area = model_area.push(
            row![
                controls::form_input(
                    "手动添加模型 ID（模型不在列表中时）",
                    &draft.map(|d| d.manual_draft.clone()).unwrap_or_default(),
                    FORM_W,
                    |v| Message::Ai(AiMsg::ProviderManualDraft(v)),
                    Message::Ai(AiMsg::ProviderManualAdd),
                ),
                controls::btn(
                    "添加",
                    &controls::btn_sm(),
                    controls::Variant::Secondary,
                    Some(Message::Ai(AiMsg::ProviderManualAdd)),
                ),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
        );
    }

    let mut col = column![].spacing(14);
    col = col.push(super::group_head("供应商", &p.id, true));
    col = col.push(field("名称", name_control));
    col = col.push(field("Base URL", url_control));
    col = col.push(field("API Key", key_control));
    col = col.push(field(format!("模型（已选 {} 个）", models.len()), model_area.into()));
    if !ro {
        col = col.push(
            container(controls::btn(
                "删除该供应商",
                &controls::btn_sm(),
                controls::Variant::Ghost,
                Some(super::open_popup(app, window, PopupState::AiProviderDeleteMenu)),
            ))
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right)
            .padding(Padding::ZERO.top(6.0)),
        );
    }
    col.into()
}
// ===== 模型服务 =====

fn models_view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let model_count = app.repo_config.providers.iter().flatten().map(|p| p.models.len()).sum::<usize>();
    let mut col = column![].spacing(8);
    if model_count == 0 {
        col = col.push(container(controls::empty_state(
            "sparkles",
            "还没有可用的模型",
            "模型清单来自供应商；先去「供应商」添加并填入模型清单",
            (!ro).then(|| (
                "去添加供应商".to_string(),
                Message::Ai(AiMsg::SubTab("providers")),
            )),
        ))
        .height(Length::Fixed(220.0)));
    }

    col = col.push(super::group_head("话题自动命名", "", true));
    let mode = naming_mode(app);
    let mode_trigger = controls::dropdown_trigger(
        naming_label(mode),
        FORM_W,
        (!ro).then(|| super::open_popup(app, window, PopupState::AiNamingMenu)),
    );
    col = col.push(super::setting_row("自动命名", "新话题首条消息后自动起标题", mode_trigger));
    if mode == NamingMode::Custom {
        let model_trigger = controls::dropdown_trigger(
            &naming_model_label(app),
            FORM_W,
            (!ro && model_count > 0).then(|| super::open_popup(app, window, PopupState::AiModelMenu)),
        );
        col = col.push(super::setting_row("命名模型", "话题命名一般用小模型；跟随所选 = 用对话时选择的模型", model_trigger));
    }
    scrollable(container(col).padding(Padding::ZERO.bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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

fn naming_label(mode: NamingMode) -> &'static str {
    match mode {
        NamingMode::Off => "不启用",
        NamingMode::FollowDefault => "跟随所选模型",
        NamingMode::Custom => "指定模型",
    }
}

fn naming_model_label(app: &App) -> String {
    match &app.repo_config.auto_naming_model {
        Some(o) => format!("{} / {}", provider_name(app, &o.provider_id), o.model),
        None => "跟随所选模型".into(),
    }
}

// ===== Agent =====

fn agents_view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let ro = readonly(app);
    let mut list = column![].spacing(4);
    for a in &app.agents {
        list = list.push(agent_card(
            app,
            window,
            a,
            app.ai.agent_sel.as_deref() == Some(a.id.as_str()),
        ));
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
        // 空态要在整列里居中，不套滚动
        None => container(controls::empty_state(
            "bot",
            "还没有配置 Agent",
            "Agent = 系统提示词 + 工具集；新建后可在对话输入区选择使用",
            (!ro).then(|| ("新建 Agent".to_string(), Message::Ai(AiMsg::AgentNew))),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        Some(a) => scrollable(
            container(agent_form(app, a, window)).padding(Padding::ZERO.left(20.0).bottom(24.0)),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    };

    row![
        container(
            scrollable(container(column![list, space().height(Length::Fixed(8.0)), new_btn]).padding(
                Padding::ZERO.right(12.0).bottom(24.0)
            ))
            .height(Length::Fill)
        )
        .width(Length::Fixed(LIST_W))
        .height(Length::Fill),
        list_divider(),
        container(right).width(Length::Fill).height(Length::Fill),
    ]
    .height(Length::Fill)
    .into()
}

fn agent_card(
    app: &App,
    window: iced::window::Id,
    a: &vault::AgentConfig,
    selected: bool,
) -> Element<'static, Message> {
    let builtin = a.builtin.unwrap_or(false);
    let summary = match &a.system_prompt_file {
        Some(f) => {
            let title = f.rsplit('/').next().unwrap_or(f).trim_end_matches(".md");
            format!("已注册提示词：{title}")
        }
        None => "无提示词".to_string(),
    };
    let mut line = row![].spacing(6).align_y(iced::alignment::Vertical::Center);
    line = line.push(
        column![
            row![
                text(a.name.clone())
                    .size(pal::fs_caption())
                    .color(if selected { pal::ACCENT } else { pal::TEXT_PRIMARY }),
                (builtin).then(|| controls::badge("预置", pal::BG_TERTIARY, pal::TEXT_SECONDARY)),
            ]
            .spacing(6)
            .align_y(iced::alignment::Vertical::Center),
            text(summary).size(pal::fs_micro()).color(pal::TEXT_MUTED),
        ]
        .spacing(2)
        .width(Length::Fill),
    );
    // 悬停显形操作钮（预置无删除）；按钮独立于选中按钮，避免行点击误触
    if app.ai.agent_hover.as_deref() == Some(a.id.as_str()) {
        line = line.push(controls::icon_button(
            "copy",
            12.0,
            22.0,
            controls::Variant::Ghost,
            (!readonly(app)).then(|| Message::Ai(AiMsg::AgentDuplicate(a.id.clone()))),
            Some((window, "复制 Agent".to_string())),
        ));
        if !builtin {
            let del = super::open_popup(app, window, PopupState::AiAgentDeleteMenu);
            line = line.push(controls::icon_button(
                "trash-2",
                12.0,
                22.0,
                controls::Variant::Ghost,
                Some(del),
                Some((window, "删除 Agent".to_string())),
            ));
        }
    }
    iced::widget::mouse_area(
        button(line)
            .width(Length::Fill)
            .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
            .style(controls::row_style(selected, pal::RADIUS_SM))
            .on_press(Message::Ai(AiMsg::AgentSelect(a.id.clone()))),
    )
    .on_enter(Message::Ai(AiMsg::AgentHover(Some(a.id.clone()))))
    .on_exit(Message::Ai(AiMsg::AgentHover(None)))
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
        controls::dropdown_trigger(&prompt_label, FORM_W, Some(super::open_popup(app, window, PopupState::AiAgentPromptMenu)))
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
        let items = category_items(app, ci);
        col = col.push(tool_category_block(app, &tools, ro, ci, cat.label, &items));
    }
    let plugin_items = category_items(app, plugin_cat_index());
    if !plugin_items.is_empty() {
        col = col.push(tool_category_block(
            app,
            &tools,
            ro,
            plugin_cat_index(),
            "插件",
            &plugin_items,
        ));
    }

    let mut actions = row![].spacing(8);
    if !builtin && !ro {
        actions = actions.push(controls::btn(
            "删除",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(super::open_popup(app, window, PopupState::AiAgentDeleteMenu)),
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

/// 一类工具的勾选块：分组头整行可点折叠，复选框只切全选 / 全不选；单工具分类不设分组头。
fn tool_category_block<'a>(
    app: &App,
    tools: &[String],
    ro: bool,
    index: usize,
    label: &str,
    items: &[(String, String)],
) -> Element<'a, Message> {
    if items.len() == 1 {
        let (id, name) = &items[0];
        let on = tools.iter().any(|x| x == id);
        let line = row![
            controls::checkbox(
                if on { controls::CheckState::On } else { controls::CheckState::Off },
                (!ro).then(|| Message::Ai(AiMsg::AgentToolToggle(id.clone()))),
            ),
            text(name.clone()).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);
        return container(line).padding(Padding::ZERO.horizontal(8.0).top(4.0)).into();
    }
    let collapsed = app.ai.collapsed_cats.get(index).copied().unwrap_or(true);
    let head = row![
        icon(
            if collapsed { "chevron-right" } else { "chevron-down" },
            12.0,
            pal::TEXT_MUTED
        ),
        controls::checkbox(state_of(tools, items), (!ro).then(|| Message::Ai(AiMsg::AgentCategoryToggle(index)))),
        text(label.to_string())
            .size(pal::fs_caption())
            .color(pal::TEXT_PRIMARY)
            .font(crate::ui::bold()),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    let mut col = column![iced::widget::mouse_area(
        container(head).padding(Padding::ZERO.horizontal(8.0).top(4.0)).width(Length::Fill),
    )
    .on_press(Message::Ai(AiMsg::AgentCatCollapse(index)))];
    if collapsed {
        return col.into();
    }
    for (id, name) in items {
        let on = tools.iter().any(|x| x == id);
        let mut tool_line = row![
            controls::checkbox(
                if on { controls::CheckState::On } else { controls::CheckState::Off },
                (!ro).then(|| Message::Ai(AiMsg::AgentToolToggle(id.clone()))),
            ),
            text(name.clone()).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);
        if id == "web_search" && !app.ai.search_ready {
            tool_line = tool_line.push(
                text("（未配置搜索源，发送时自动降级）")
                    .size(pal::fs_micro())
                    .color(pal::TEXT_MUTED),
            );
        }
        col = col.push(container(tool_line).padding(Padding::ZERO.left(20.0)));
    }
    col.into()
}

fn state_of(tools: &[String], items: &[(String, String)]) -> controls::CheckState {
    let hit = items.iter().filter(|(id, _)| tools.iter().any(|x| x == id)).count();
    if hit == 0 {
        controls::CheckState::Off
    } else if hit == items.len() {
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
        controls::dropdown_trigger(provider_label, FORM_W, Some(super::open_popup(app, window, PopupState::AiSearchProviderMenu)))
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
    scrollable(container(col).padding(Padding::ZERO.bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
