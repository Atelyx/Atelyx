//! 表格 AI 字段：逐行生成队列、自动触发与过期标记。
//! 值写入走表格会话（撤销栈 / 置脏 / 保存链），网络调用在独立任务里做、回执按批次与指纹守卫。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use iced::widget::text_editor;
use iced::widget::text_editor::Action;
use iced::{Point, Task};

use atelyx_core::table_ai as ai;
use atelyx_core::vault::{self, AiCellMeta, AiFieldConfig, AiSystem, TableField, TableRow};

use crate::layout_view::{PopupOpen, PopupState};
use crate::table_view as tv;
use crate::{App, Message, RepoKind};

/// 同时在途的生成请求数（成本与吞吐折中）。
const CONCURRENCY: usize = 3;
/// 系统提示词正文上限（字符；笔记过长时截断，避免超出上下文）。
const SYSTEM_NOTE_CHAR_CAP: usize = 16_000;

/// 系统提示词编辑形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiSystemMode {
    None,
    Inline,
    Note,
}

/// 配置弹层草稿。
pub struct AiDraft {
    pub field_id: String,
    pub system_mode: AiSystemMode,
    pub system_text: text_editor::Content,
    pub note: String,
    pub prompt: text_editor::Content,
    /// 生成模型（供应商 id + 模型 id；None = 跟随仓库默认模型）。
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub auto_new: bool,
    pub auto_source: bool,
    pub show_fields: bool,
    pub show_models: bool,
}

/// 一次生成批次的固定输入（配置在发起时冻结；提交前按指纹复核）。
pub struct AiBatch {
    pub id: u64,
    pub field_id: String,
    pub file: String,
    pub gen: u64,
    pub queue: VecDeque<String>,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 首个失败原因（批次汇总展示，便于不逐格查看）。
    pub first_error: Option<String>,
    pub running: usize,
    pub cancelled: bool,
    pub pushed_undo: bool,
    pub spec: AiRunSpec,
    /// 发起时冻结的字段配置（在途中改配置 = 结果作废）。
    pub config: AiFieldConfig,
    /// 发起时各待生成格的取值（行 id → 值）：批次期间被改写（手动编辑 / 远端补丁）的格
    /// 不再派发，回执也不落地——用户内容优先于模型输出。
    pub baseline: HashMap<String, Option<serde_json::Value>>,
    pub client: atelyx_core::ai::AiClient,
}

#[derive(Clone)]
pub struct AiRunSpec {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub system: Option<String>,
    pub max_tokens: u32,
}

/// 空间模式下系统提示词笔记的预取中转。
struct PendingStart {
    gen: u64,
    file: String,
    field_id: String,
    rows: Vec<String>,
    config: AiFieldConfig,
    note: String,
}

#[derive(Default)]
pub struct TableAiState {
    /// 在途单元格（field_id, row_id）。
    pub runs: HashSet<(String, String)>,
    pub batch: Option<AiBatch>,
    next_batch_id: u64,
    pub draft: Option<AiDraft>,
    pending: Option<PendingStart>,
    /// 每个 AI 字段的模板引用（布局重建时解析，绘制只读）。
    pub refs: HashMap<String, Vec<ai::AiRef>>,
    /// 已生成但输入已变的格（field_id → row_id）。
    pub stale: HashMap<String, HashSet<String>>,
    /// 待自动生成（来源变更 / 新增行）。
    pub auto_due: BTreeSet<(String, String)>,
    /// 本端新增、尚未生成过结果的行（「新增行时生成」的触发范围）。
    pub fresh_rows: HashSet<String>,
}

impl TableAiState {
    fn next_id(&mut self) -> u64 {
        self.next_batch_id += 1;
        self.next_batch_id
    }
}

/// 打开字段配置弹层。
pub fn open_config(app: &mut App, window: iced::window::Id, field_id: &str) -> Task<Message> {
    let Some(session) = app.table.as_ref() else { return Task::none() };
    let Some(field) = session.data.fields.iter().find(|f| f.id == field_id) else {
        return Task::none();
    };
    if !ai::is_ai(field) {
        return Task::none();
    }
    let draft = draft_for(field);
    app.dismiss_popup();
    if let Some(session) = app.table.as_mut() {
        session.view.ai.draft = Some(draft);
    }
    app.popup = Some((
        window,
        PopupOpen {
            point: Point::ORIGIN,
            flip_y: None,
            state: PopupState::TableAiField { field_id: field_id.to_string() },
            min_width: 0.0,
        },
    ));
    iced::widget::operation::focus(iced::widget::Id::new(crate::table_ai_view::PROMPT_EDITOR_ID))
        .map(|()| Message::Table(tv::TableMsg::Noop))
}

fn draft_for(field: &TableField) -> AiDraft {
    let config = field.ai.clone().unwrap_or_default();
    let (system_mode, system_text, note) = match &config.system {
        AiSystem::None => (AiSystemMode::None, String::new(), String::new()),
        AiSystem::Inline { text } => (AiSystemMode::Inline, text.clone(), String::new()),
        AiSystem::Note { path } => (AiSystemMode::Note, String::new(), path.clone()),
    };
    AiDraft {
        field_id: field.id.clone(),
        system_mode,
        system_text: text_editor::Content::with_text(&system_text),
        note,
        prompt: text_editor::Content::with_text(&config.prompt),
        provider_id: config.provider_id.clone(),
        model: config.model.clone(),
        auto_new: config.auto_new_row,
        auto_source: config.auto_source_change,
        show_fields: false,
        show_models: false,
    }
}

/// 弹层内插入字段引用（在光标处写 #字段名）。
pub fn insert_ref(app: &mut App, field_id: &str) {
    let name = match app.table.as_ref().and_then(|session| {
        let _draft = session.view.ai.draft.as_ref()?;
        session.data.fields.iter().find(|f| f.id == field_id).map(|f| f.name.clone())
    }) {
        Some(name) => name,
        None => return,
    };
    if let Some(session) = app.table.as_mut() {
        if let Some(draft) = session.view.ai.draft.as_mut() {
            let text = draft.prompt.text();
            let cursor = draft.prompt.cursor().position;
            // 未在模板里落过光标 = 插入点仍在文首：追加到末尾更符合「继续补一句」的预期
            if !text.is_empty() && cursor.line == 0 && cursor.column == 0 {
                draft.prompt.perform(Action::Move(text_editor::Motion::DocumentEnd));
            }
            draft.prompt.perform(Action::Edit(text_editor::Edit::Paste(Arc::new(format!("#{name}")))));
            draft.show_fields = false;
        }
    }
}

/// 保存草稿：允许留下未填项（配置可先存下，缺项在生成 / 更新时再报错）。
pub fn save_draft(app: &mut App) -> Task<Message> {
    if matches!(&app.repo_kind, RepoKind::Space(space) if !space.role.can_write()) {
        app.notify(crate::ui::notify::Level::Warning, "该空间为只读身份，无法修改 AI 字段");
        return Task::none();
    }
    let Some(session) = app.table.as_ref() else { return Task::none() };
    let Some(draft) = session.view.ai.draft.as_ref() else { return Task::none() };
    let field_id = draft.field_id.clone();
    let Some(field) = session.data.fields.iter().find(|f| f.id == field_id) else {
        return Task::none();
    };
    let config = config_from_draft(draft);
    let changed = field.ai.as_ref() != Some(&config);
    let mut stopped = false;
    if changed {
        if let Some(session) = app.table.as_mut() {
            let snapshot = (session.data.fields.clone(), session.data.rows.clone());
            session.view.undo.push(snapshot);
            if session.view.undo.len() > tv::UNDO_DEPTH {
                session.view.undo.remove(0);
            }
            if let Some(target) = session.data.fields.iter_mut().find(|f| f.id == field_id) {
                target.ai = Some(config.clone());
            }
            // 影响输出的配置一变，在途批次按旧配置产出的结果作废：停掉未派发的行
            if let Some(batch) = session.view.ai.batch.as_mut() {
                let same_field = batch.field_id == field_id;
                if same_field && !ai::same_output_config(&batch.config, &config) {
                    batch.cancelled = true;
                    batch.queue.clear();
                    stopped = true;
                }
            }
            if stopped && session.view.ai.batch.as_ref().is_some_and(|batch| batch.running == 0) {
                session.view.ai.batch = None;
                session.view.ai.runs.clear();
            }
            session.touch();
        }
    }
    if stopped {
        app.notify(crate::ui::notify::Level::Warning, "配置已更新，进行中的生成已停止");
    }
    if let Some(session) = app.table.as_mut() {
        session.view.ai.draft = None;
    }
    app.dismiss_popup();
    Task::none()
}

fn config_from_draft(draft: &AiDraft) -> AiFieldConfig {
    let system = match draft.system_mode {
        AiSystemMode::None => AiSystem::None,
        AiSystemMode::Inline => AiSystem::Inline { text: draft.system_text.text() },
        AiSystemMode::Note => AiSystem::Note { path: draft.note.clone() },
    };
    AiFieldConfig {
        system,
        prompt: draft.prompt.text(),
        provider_id: draft.provider_id.clone(),
        model: draft.model.clone(),
        auto_new_row: draft.auto_new,
        auto_source_change: draft.auto_source,
    }
}

/// 列内的生成入口：按筛选口径取行（全部 / 空值 / 待更新 / 失败）。
/// 配置合法性在这里报错：保存配置不拦缺项，生成时才要求填全。
pub fn start_filtered(app: &mut App, field_id: &str, filter: ai::AiRowFilter) -> Task<Message> {
    let picked = match app.table.as_ref() {
        Some(session) => match session.data.fields.iter().find(|f| f.id == field_id) {
            Some(field) => ai::validate(field, &session.data.fields).map(|_| {
                ai::filter_rows(field, &session.data.fields, &session.data.rows, filter)
            }),
            None => Ok(Vec::new()),
        },
        None => Ok(Vec::new()),
    };
    let rows = match picked {
        Ok(rows) => rows,
        Err(error) => {
            app.notify(crate::ui::notify::Level::Warning, error);
            return Task::none();
        }
    };
    if rows.is_empty() {
        app.notify(crate::ui::notify::Level::Warning, "没有可生成的行（引用列为空或配置不完整）");
        return Task::none();
    }
    start_rows(app, field_id, rows)
}

/// 指定行生成。
pub fn start_rows(app: &mut App, field_id: &str, rows: Vec<String>) -> Task<Message> {
    if matches!(&app.repo_kind, RepoKind::Space(space) if !space.role.can_write()) {
        app.notify(crate::ui::notify::Level::Warning, "该空间为只读身份，无法生成");
        return Task::none();
    }
    let Some(session) = app.table.as_ref() else { return Task::none() };
    if session.view.ai.batch.is_some() {
        app.notify(crate::ui::notify::Level::Warning, "上一批还在生成，先停止再试");
        return Task::none();
    }
    let Some(field) = session.data.fields.iter().find(|f| f.id == field_id) else {
        return Task::none();
    };
    let refs = match ai::validate(field, &session.data.fields) {
        Ok(refs) => refs,
        Err(error) => {
            app.notify(crate::ui::notify::Level::Warning, error);
            return Task::none();
        }
    };
    let rows: Vec<String> = rows
        .into_iter()
        .filter(|row_id| {
            session
                .data
                .rows
                .iter()
                .find(|r| &r.id == row_id)
                .is_some_and(|row| !ai::no_input(&session.data.fields, &refs, row))
        })
        .collect();
    if rows.is_empty() {
        app.notify(crate::ui::notify::Level::Warning, "引用的列为空，未生成");
        return Task::none();
    }
    let config = match field.ai.clone() {
        Some(config) => config,
        None => return Task::none(),
    };
    match &config.system {
        AiSystem::None => launch(app, field_id, rows, None),
        AiSystem::Inline { text } => {
            let system = (!text.trim().is_empty()).then(|| text.clone());
            launch(app, field_id, rows, system)
        }
        AiSystem::Note { path } => match &app.repo_kind {
            RepoKind::Local => {
                let Some(root) = app.vault.root().ok() else {
                    app.notify(crate::ui::notify::Level::Error, "仓库未打开，无法读取系统提示词笔记");
                    return Task::none();
                };
                match vault::read_note(&root, path) {
                    Ok(text) => launch(app, field_id, rows, Some(text)),
                    Err(error) => {
                        app.notify(
                            crate::ui::notify::Level::Error,
                            format!("系统提示词笔记读取失败（{path}）：{error}"),
                        );
                        Task::none()
                    }
                }
            }
            RepoKind::Space(space) => {
                let gen = app.epoch();
                let client = space.client.clone();
                let space_id = space.space_id.clone();
                let note = path.clone();
                let file = session.file.clone();
                if let Some(session) = app.table.as_mut() {
                    session.view.ai.pending = Some(PendingStart {
                        gen,
                        file,
                        field_id: field_id.to_string(),
                        rows,
                        config: config.clone(),
                        note: note.clone(),
                    });
                }
                let note_for_msg = note.clone();
                Task::perform(
                    async move { client.get_file_text(&space_id, &note).await.map_err(|e| e.to_string()) },
                    move |result| {
                        Message::Table(tv::TableMsg::AiSystemReady {
                            gen,
                            path: note_for_msg.clone(),
                            result,
                        })
                    },
                )
            }
        },
    }
}

/// 空间笔记预取回执：成功则继续发起批次，失败整批不跑。
pub fn on_system_ready(app: &mut App, gen: u64, path: String, result: Result<String, String>) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    let Some(session) = app.table.as_mut() else { return Task::none() };
    let Some(pending) = session.view.ai.pending.as_ref() else { return Task::none() };
    // 回执按笔记路径认领：两个 Note 字段先后预取时，先到的正文不得套到后一个字段上
    if pending.note != path {
        return Task::none();
    }
    let Some(pending) = session.view.ai.pending.take() else { return Task::none() };
    if pending.file != session.file || pending.gen != gen {
        return Task::none();
    }
    let same_config = session
        .data
        .fields
        .iter()
        .find(|field| field.id == pending.field_id)
        .and_then(|field| field.ai.as_ref())
        .is_some_and(|config| ai::same_output_config(config, &pending.config));
    if !same_config {
        app.notify(crate::ui::notify::Level::Warning, "字段配置已变化，请重新生成");
        return Task::none();
    }
    match result {
        Ok(text) => launch(app, &pending.field_id, pending.rows, Some(text)),
        Err(error) => {
            app.notify(crate::ui::notify::Level::Error, format!("系统提示词笔记读取失败：{error}"));
            Task::none()
        }
    }
}

/// 起批次：解析模型 / 客户端并登记，然后立即泵出首批请求。
pub fn launch(app: &mut App, field_id: &str, rows: Vec<String>, system: Option<String>) -> Task<Message> {
    let Some(session) = app.table.as_ref() else { return Task::none() };
    let Some(field) = session.data.fields.iter().find(|f| f.id == field_id) else {
        return Task::none();
    };
    let Some(config) = field.ai.clone() else { return Task::none() };
    if session.view.ai.batch.is_some() {
        return Task::none();
    }
    let (provider_id, model) = match config_model(app, &config) {
        Ok(pick) => pick,
        Err(error) => {
            app.notify(crate::ui::notify::Level::Warning, error);
            return Task::none();
        }
    };
    let run = match crate::chat_actions::resolve_run_config(
        app,
        None,
        None,
        false,
        Some((provider_id.as_str(), model.as_str())),
    ) {
        Ok(run) => run,
        Err(error) => {
            app.notify(crate::ui::notify::Level::Error, error);
            return Task::none();
        }
    };
    let max_tokens = atelyx_core::ai::table_field::MAX_TOKENS;
    let Some(client) = atelyx_core::ai::agent::aid_client(&run.base_url) else {
        app.notify(crate::ui::notify::Level::Error, "无法建立模型连接，请检查网络或供应商地址");
        return Task::none();
    };
    let system = system.map(|text| text.chars().take(SYSTEM_NOTE_CHAR_CAP).collect::<String>());
    let file = session.file.clone();
    let gen = app.epoch();
    let baseline: HashMap<String, Option<serde_json::Value>> = rows
        .iter()
        .filter_map(|id| {
            session
                .data
                .rows
                .iter()
                .find(|r| &r.id == id)
                .map(|row| (id.clone(), row.values.get(field_id).cloned()))
        })
        .collect();
    let Some(session) = app.table.as_mut() else { return Task::none() };
    let id = session.view.ai.next_id();
    let total = rows.len();
    let spec = AiRunSpec {
        base_url: run.base_url,
        api_key: run.api_key,
        model: run.model,
        system,
        max_tokens,
    };
    session.view.ai.batch = Some(AiBatch {
        id,
        field_id: field_id.to_string(),
        file,
        gen,
        queue: rows.into(),
        total,
        done: 0,
        failed: 0,
        skipped: 0,
        first_error: None,
        running: 0,
        cancelled: false,
        pushed_undo: false,
        spec,
        config,
        baseline,
        client,
    });
    pump(app)
}

/// 字段指定的模型优先；未指定回落仓库默认模型。
fn config_model(app: &App, config: &AiFieldConfig) -> Result<(String, String), String> {
    match (config.provider_id.as_deref(), config.model.as_deref()) {
        (Some(provider), Some(model)) if !provider.trim().is_empty() && !model.trim().is_empty() => {
            Ok((provider.to_string(), model.to_string()))
        }
        _ => match (&app.repo_config.model, &app.repo_config.model_provider_id) {
            (Some(model), Some(provider))
                if !model.trim().is_empty() && !provider.trim().is_empty() =>
            {
                Ok((provider.clone(), model.clone()))
            }
            _ => Err("未设置模型：在 AI 字段设置里选择模型，或在「设置 → AI」里设置默认模型"
                .to_string()),
        },
    }
}

/// 泵出请求：保持至多 CONCURRENCY 个在途，回执按批次 id、输入指纹与格值守卫。
pub fn pump(app: &mut App) -> Task<Message> {
    // 发起后被改写的格（手动编辑 / 远端补丁）不再派发：用户内容优先于模型输出
    let (dropped, settled) = {
        let Some(session) = app.table.as_mut() else { return Task::none() };
        let Some(batch) = session.view.ai.batch.as_mut() else { return Task::none() };
        if batch.cancelled {
            return Task::none();
        }
        let field_id = batch.field_id.clone();
        let rows = &session.data.rows;
        let mut queue = std::mem::take(&mut batch.queue);
        let mut dropped = 0usize;
        queue.retain(|row_id| {
            let same = batch.baseline.get(row_id).is_some_and(|before| {
                rows.iter()
                    .find(|row| &row.id == row_id)
                    .is_some_and(|row| {
                        atelyx_core::table_ops::cell_value_equal(
                            row.values.get(&field_id),
                            before.as_ref(),
                        )
                    })
            });
            if !same {
                dropped += 1;
            }
            same
        });
        batch.queue = queue;
        batch.skipped += dropped;
        (dropped, batch.queue.is_empty() && batch.running == 0)
    };
    if dropped > 0 && settled {
        return finish(app);
    }
    // 队列里已没有任何「行还在 + 配置可算指纹」的项：清空并收尾，避免批次空转
    let dispatchable = {
        let Some(session) = app.table.as_ref() else { return Task::none() };
        let Some(batch) = session.view.ai.batch.as_ref() else { return Task::none() };
        batch.cancelled
            || batch.queue.iter().any(|id| {
                session
                    .data
                    .rows
                    .iter()
                    .find(|row| &row.id == id)
                    .is_some_and(|row| {
                        ai::fingerprint_with_config(&batch.config, &session.data.fields, row).is_some()
                    })
            })
    };
    if !dispatchable {
        if let Some(session) = app.table.as_mut() {
            if let Some(batch) = session.view.ai.batch.as_mut() {
                batch.queue.clear();
            }
        }
        return finish(app);
    }
    // 配置在途中被改：未派发的行不再按旧配置发起（已派发的回执会按配置不一致丢弃）
    let config_same = {
        let Some(session) = app.table.as_ref() else { return Task::none() };
        let Some(batch) = session.view.ai.batch.as_ref() else { return Task::none() };
        session
            .data
            .fields
            .iter()
            .find(|field| field.id == batch.field_id)
            .and_then(|field| field.ai.as_ref())
            .is_some_and(|config| ai::same_output_config(config, &batch.config))
    };
    if !config_same {
        if let Some(session) = app.table.as_mut() {
            if let Some(batch) = session.view.ai.batch.as_mut() {
                batch.queue.clear();
            }
        }
        return finish(app);
    }
    let Some(session) = app.table.as_mut() else { return Task::none() };
    let Some(batch) = session.view.ai.batch.as_ref() else { return Task::none() };
    if batch.cancelled {
        return Task::none();
    }
    let need = CONCURRENCY.saturating_sub(batch.running);
    if need == 0 || batch.queue.is_empty() {
        return Task::none();
    }
    let fields = &session.data.fields;
    let rows = &session.data.rows;
    let mut picks: Vec<(String, String, String)> = Vec::new();
    {
        let batch = session.view.ai.batch.as_ref().expect("batch 已判存在");
        for row_id in batch.queue.iter() {
            if picks.len() >= need {
                break;
            }
            let Some(row) = rows.iter().find(|r| &r.id == row_id) else { continue };
            let Some(input) = ai::fingerprint_with_config(&batch.config, fields, row) else { continue };
            picks.push((row.id.clone(), input, ai::render(&batch.config.prompt, fields, row)));
        }
    }
    if picks.is_empty() {
        if let Some(batch) = session.view.ai.batch.as_mut() {
            batch.queue.clear();
        }
        return Task::none();
    }
    let mut tasks = Vec::with_capacity(picks.len());
    let Some(batch) = session.view.ai.batch.as_mut() else { return Task::none() };
    // 只摘掉真正派发的行：队首可能是已删除 / 无法算指纹的行，按数量 pop 会误删后续行并重复派发
    let picked: HashSet<String> = picks.iter().map(|(row_id, _, _)| row_id.clone()).collect();
    let field_key = batch.field_id.clone();
    batch.queue.retain(|id| !picked.contains(id));
    batch.running += picks.len();
    for row_id in &picked {
        session.view.ai.auto_due.remove(&(field_key.clone(), row_id.clone()));
    }
    for (row_id, input, user) in picks {
        session.view.ai.runs.insert((row_id.clone(), batch.field_id.clone()));
        let spec = batch.spec.clone();
        let id = batch.id;
        let field_id = batch.field_id.clone();
        let client = batch.client.clone();
        tasks.push(Task::perform(
            async move {
                atelyx_core::ai::table_field::generate(
                    &client,
                    &spec.base_url,
                    &spec.api_key,
                    &spec.model,
                    spec.system,
                    user,
                    spec.max_tokens,
                )
                .await
            },
            move |result| {
                Message::Table(tv::TableMsg::AiCellDone {
                    batch: id,
                    field_id: field_id.clone(),
                    row_id: row_id.clone(),
                    input: input.clone(),
                    result,
                })
            },
        ));
    }
    Task::batch(tasks)
}
/// 单格回执：配置与输入未变才落值；失败落失败态并保留旧值。
pub fn on_cell_done(
    app: &mut App,
    batch_id: u64,
    field_id: String,
    row_id: String,
    input: String,
    result: Result<String, String>,
) -> Task<Message> {
    let mut snapshot_to_push = None;
    let mut commit: Option<(bool, String)> = None;
    let cancelled;
    let gen = app.epoch();
    {
        let Some(session) = app.table.as_mut() else { return Task::none() };
        let Some(batch) = session.view.ai.batch.as_mut() else { return Task::none() };
        if batch.id != batch_id || batch.field_id != field_id || batch.gen != gen {
            return Task::none();
        }
        session.view.ai.runs.remove(&(row_id.clone(), field_id.clone()));
        batch.running = batch.running.saturating_sub(1);
        cancelled = batch.cancelled;
        if !cancelled {
            let field = session.data.fields.iter().find(|f| f.id == field_id);
            let same_config = field
                .and_then(|field| field.ai.as_ref())
                .is_some_and(|config| ai::same_output_config(config, &batch.config));
            // 发起后该格又被写过（手动编辑 / 远端补丁）＝ 结果作废，不用模型输出覆盖用户内容
            let row = session.data.rows.iter().find(|r| r.id == row_id);
            let value_same = row.is_some_and(|row| {
                batch.baseline.get(&row_id).is_some_and(|before| {
                    atelyx_core::table_ops::cell_value_equal(row.values.get(&field_id), before.as_ref())
                })
            });
            let matches = same_config
                && value_same
                && field
                    .zip(row)
                    .and_then(|(_, row)| {
                        ai::fingerprint_with_config(&batch.config, &session.data.fields, row)
                    })
                    .as_deref()
                    == Some(input.as_str());
            if !matches {
                batch.skipped += 1;
            } else {
                if !batch.pushed_undo {
                    // 快照在首笔写入前才取：批次在途期间用户的其它编辑不被卷进本次撤销步
                    snapshot_to_push = Some((session.data.fields.clone(), session.data.rows.clone()));
                    batch.pushed_undo = true;
                }
                match result {
                    Ok(text) => {
                        batch.done += 1;
                        commit = Some((true, text));
                    }
                    Err(error) => {
                        batch.failed += 1;
                        if batch.first_error.is_none() {
                            batch.first_error = Some(error.clone());
                        }
                        commit = Some((false, error));
                    }
                }
            }
        }
    }
    if cancelled {
        return finish(app);
    }
    if let Some(snapshot) = snapshot_to_push {
        if let Some(session) = app.table.as_mut() {
            push_snapshot(session, snapshot);
        }
    }
    if let Some((ok, body)) = commit {
        let model = app
            .table
            .as_ref()
            .and_then(|s| s.view.ai.batch.as_ref())
            .map(|b| b.spec.model.clone());
        if let Some(session) = app.table.as_mut() {
            let at = vault::now_secs();
            if let Some(row) = session.data.rows.iter_mut().find(|r| r.id == row_id) {
                let meta = if ok {
                    AiCellMeta::ok(input, at, model)
                } else {
                    AiCellMeta::failed(input, at, model, body.clone())
                };
                if ok {
                    row.values.insert(field_id.clone(), serde_json::Value::String(body));
                }
                row.ai.get_or_insert_with(Default::default).insert(field_id.clone(), meta);
            }
            if ok {
                session.view.ai.fresh_rows.remove(&row_id);
                session.view.ai.auto_due.remove(&(field_id.clone(), row_id.clone()));
            }
            session.touch();
        }
    }
    finish(app)
}

fn push_snapshot(session: &mut tv::TableSession, snapshot: (Vec<TableField>, Vec<TableRow>)) {
    session.view.undo.push(snapshot);
    if session.view.undo.len() > tv::UNDO_DEPTH {
        session.view.undo.remove(0);
    }
}

/// 批次收尾：队列空且无在途时汇总提示并清批次，随后续跑自动队列。
pub fn finish(app: &mut App) -> Task<Message> {
    let (queue_left, running) = {
        let Some(session) = app.table.as_ref() else { return Task::none() };
        match session.view.ai.batch.as_ref() {
            Some(batch) => (!batch.queue.is_empty(), batch.running),
            None => (false, 0),
        }
    };
    if running > 0 && !queue_left {
        // 队列已空、还有在途：等回执收尾（再调 pump 会因无可派发行而空转）
        return Task::none();
    }
    if queue_left {
        return pump(app);
    }
    let summary = {
        let Some(session) = app.table.as_mut() else { return Task::none() };
        let Some(batch) = session.view.ai.batch.take() else { return Task::none() };
        session.view.ai.runs.clear();
        (
            batch.done,
            batch.failed,
            batch.skipped,
            batch.cancelled,
            batch.total,
            batch.first_error,
        )
    };
    let (done, failed, skipped, cancelled, total, first_error) = summary;
    if cancelled {
        app.notify(crate::ui::notify::Level::Warning, format!("已停止生成（完成 {done}/{total} 格）"));
    } else if failed == 0 && skipped == 0 {
        app.notify(crate::ui::notify::Level::Success, format!("AI 生成完成：{done} 格"));
    } else {
        let level = if done == 0 && failed > 0 {
            crate::ui::notify::Level::Error
        } else {
            crate::ui::notify::Level::Warning
        };
        let reason = first_error
            .map(|error| {
                let short: String = error.chars().take(80).collect();
                format!("（首个原因：{short}）")
            })
            .unwrap_or_default();
        app.notify(level, format!("AI 生成结束：成功 {done} · 失败 {failed} · 跳过 {skipped}{reason}"));
    }
    drain_auto(app)
}

/// 停止当前批次：不再发起新请求；在途回执返回后丢弃。
pub fn stop(app: &mut App) {
    let finished = {
        let Some(session) = app.table.as_mut() else { return };
        let Some(batch) = session.view.ai.batch.as_mut() else { return };
        batch.cancelled = true;
        batch.queue.clear();
        if batch.running == 0 {
            session.view.ai.batch = None;
            session.view.ai.runs.clear();
            true
        } else {
            false
        }
    };
    if finished {
        app.notify(crate::ui::notify::Level::Warning, "已停止生成");
    } else {
        app.notify(crate::ui::notify::Level::Warning, "正在停止：在途请求返回后丢弃");
    }
}

/// 新增行登记自动生成（仅用户交互新增的行）。
pub fn note_new_rows(app: &mut App, row_ids: &[String]) {
    let Some(session) = app.table.as_ref() else { return };
    let fields = &session.data.fields;
    let mut due = Vec::new();
    for field in fields.iter().filter(|f| f.ai.as_ref().is_some_and(|c| c.auto_new_row)) {
        let Ok(refs) = ai::validate(field, fields) else { continue };
        for row_id in row_ids {
            let Some(row) = session.data.rows.iter().find(|r| &r.id == row_id) else { continue };
            if ai::no_input(fields, &refs, row) {
                continue;
            }
            let has_value = row
                .values
                .get(&field.id)
                .and_then(|v| v.as_str())
                .is_some_and(|text| !text.is_empty());
            if !has_value {
                due.push((field.id.clone(), row_id.clone()));
            }
        }
    }
    if let Some(session) = app.table.as_mut() {
        for row_id in row_ids {
            session.view.ai.fresh_rows.insert(row_id.clone());
        }
        for entry in due {
            session.view.ai.auto_due.insert(entry);
        }
    }
}

/// 自动队列：无活动批次时取一个字段的待生成行开批（一次只跑一个字段）。
pub fn drain_auto(app: &mut App) -> Task<Message> {
    if app.table.as_ref().is_none_or(|s| s.view.ai.batch.is_some()) {
        return Task::none();
    }
    if app.repo_config.model.is_none() || app.repo_config.model_provider_id.is_none() {
        return Task::none();
    }
    let Some(session) = app.table.as_ref() else { return Task::none() };
    let Some((field_id, _)) = session.view.ai.auto_due.iter().next().cloned() else {
        return Task::none();
    };
    let rows: Vec<String> = session
        .view
        .ai
        .auto_due
        .iter()
        .filter(|(field, _)| field == &field_id)
        .map(|(_, row)| row.clone())
        .collect();
    if rows.is_empty() {
        return Task::none();
    }
    if let Some(session) = app.table.as_mut() {
        for row in &rows {
            session.view.ai.auto_due.remove(&(field_id.clone(), row.clone()));
        }
    }
    let rows = {
        let Some(session) = app.table.as_ref() else { return Task::none() };
        let Some(field) = session.data.fields.iter().find(|f| f.id == field_id) else {
            return Task::none();
        };
        let Ok(refs) = ai::validate(field, &session.data.fields) else {
            return Task::none();
        };
        rows.into_iter()
            .filter(|row_id| {
                session
                    .data
                    .rows
                    .iter()
                    .find(|row| &row.id == row_id)
                    .is_some_and(|row| !ai::no_input(&session.data.fields, &refs, row))
            })
            .collect::<Vec<_>>()
    };
    if rows.is_empty() {
        return Task::none();
    }
    start_rows(app, &field_id, rows)
}

/// 撤销前取消在途批次（回执丢弃）；没有在途请求时立即收尾，避免留一个永远不结束的空批次。
pub fn cancel_before_undo(app: &mut App) {
    let Some(session) = app.table.as_mut() else { return };
    let Some(batch) = session.view.ai.batch.as_mut() else { return };
    batch.cancelled = true;
    batch.queue.clear();
    if batch.running == 0 {
        session.view.ai.batch = None;
        session.view.ai.runs.clear();
    }
}

/// 本地编辑某格后：引用该列的 AI 字段（开了「引用变化自动更新」且该行已有结果）排入自动队列。
pub fn note_source_edit(app: &mut App, row_id: &str, field_id: &str) {
    note_source_rows(app, &[row_id.to_string()], &[field_id.to_string()]);
}

/// 本地批量编辑：rows × changed_fields 命中引用关系的行排入自动队列。
pub fn note_source_rows(app: &mut App, rows: &[String], changed_fields: &[String]) {
    let Some(session) = app.table.as_ref() else { return };
    let fields = &session.data.fields;
    let mut due = Vec::new();
    for target in fields.iter().filter(|f| {
        f.ai
            .as_ref()
            .is_some_and(|c| c.auto_source_change || c.auto_new_row)
    }) {
        let auto_source = target.ai.as_ref().is_some_and(|c| c.auto_source_change);
        let auto_new = target.ai.as_ref().is_some_and(|c| c.auto_new_row);
        let prompt = target.ai.as_ref().map(|c| c.prompt.clone()).unwrap_or_default();
        let Ok(refs) = ai::parse_refs(&prompt, fields) else { continue };
        if !refs.iter().any(|r| changed_fields.contains(&r.field_id)) {
            continue;
        }
        for row_id in rows {
            let Some(row) = session.data.rows.iter().find(|r| &r.id == row_id) else { continue };
            let has_value = row
                .values
                .get(&target.id)
                .and_then(|v| v.as_str())
                .is_some_and(|text| !text.is_empty());
            let fresh = session.view.ai.fresh_rows.contains(row_id);
            // 引用列变化只在该行输入确实与已生成指纹不一致时排入，避免叉积式重复生成
            let needs_refresh = has_value && ai::stale(target, fields, row);
            if (auto_source && needs_refresh) || (auto_new && !has_value && fresh) {
                due.push((target.id.clone(), row_id.clone()));
            }
        }
    }
    if let Some(session) = app.table.as_mut() {
        for entry in due {
            session.view.ai.auto_due.insert(entry);
        }
    }
}
/// 选区若恰好落在一个 AI 字段列上，返回该字段与涉及的行（右键「更新」用）。
pub fn selected_field(app: &App) -> Option<(String, Vec<String>)> {
    let session = app.table.as_ref()?;
    let region = atelyx_core::table_ops::region_of(
        session.view.selection.as_ref(),
        &session.data.fields,
        &session.data.rows,
    )?;
    let mut fields = session.data.fields[region.col_start..=region.col_end]
        .iter()
        .filter(|field| ai::is_ai(field));
    let field = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let rows = session.data.rows[region.row_start..=region.row_end]
        .iter()
        .map(|row| row.id.clone())
        .collect();
    Some((field.id.clone(), rows))
}
/// 配置弹层草稿的可变访问（未打开时忽略）。
pub fn edit_draft(app: &mut App, edit: impl FnOnce(&mut AiDraft)) {
    if let Some(session) = app.table.as_mut() {
        if let Some(draft) = session.view.ai.draft.as_mut() {
            edit(draft);
        }
    }
}
