//! 笔记插件：只读 / 预览编辑 / 源码三档。正文真源在本端缓冲，落盘经宿主按磁盘基线
//! 校验的保存协议，协作经宿主的房间通道收发。

mod collab;
mod edit;
mod model;
mod session;
mod ui;

use std::sync::{Arc, Mutex};

use atelyx_format::frontmatter::{self, Yaml};

use atelyx_plugin_protocol::event::{
    self, DocChangedPayload, DocRestorePayload, FileChangeKind, FileChangedPayload, PublishParams,
};
use atelyx_plugin_protocol::frame::{FrameInput, FrameInputParams, FrameTickParams, Key, KeyAction};
use atelyx_plugin_protocol::method::{
    content as content_method, CollabMessageParams, ContentPropsReadParams, ContentPropsResult,
    ContentPropsWriteParams, ContributionSlot, DocInstanceCloseParams, DocInstanceOpenParams,
    EditorPrefs, HistoryRecordParams, NotificationLevel, PanelFlushDoneParams, PanelFlushParams,
    SurfaceAttachedParams,
};
use atelyx_plugin_protocol::service::HostCallParams;
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use atelyx_plugin_props as props;

use crate::model::{Backlink, Mode, NoteModel, PANEL, SOURCE, WikiSuggest};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.notes";

/// 单个文档一次保存操作内允许的合流重试次数：磁盘被持续外部改写时以可见失败收场。
const SAVE_ATTEMPTS: usize = 3;

/// 历史版本合并窗口：窗口内的连续保存并成一版，与表格侧同一口径。
const HISTORY_COALESCE_MS: i64 = 60_000;

/// 挂着协作文档时的帧拍频率：在场保活与对端剔除按拍推进，静止时不需要更高。
const COLLAB_TICK_HZ: u16 = 4;

/// 待揭示单元的拖动阈值（px）：按下位移超过它 = 跨块拖选，不揭示（与控件层拖拽判定同档）。
const REVEAL_DRAG_THRESHOLD: f32 = 4.0;

/// 已有在途保存时的可见原因：此刻的正文还没有落盘目标，不能假装成功。
const SAVE_BUSY: &str = "文档正在保存，稍后重试";

/// 等在途保存落地的间隔与轮数：同一文档不允许并发写，短暂等待比直接失败更少打扰。
const SAVE_BUSY_WAIT: std::time::Duration = std::time::Duration::from_millis(20);
const SAVE_BUSY_WAIT_ROUNDS: usize = 10;

/// 磁盘改动与本端改动无法安全合并时的可见原因：两侧内容都保留，不自动选边。
/// 撤销回到打开时的正文后重试即可接纳磁盘内容（撤销栈本身就是出口）。
const SAVE_CONFLICT: &str = "磁盘已被外部修改且无法自动合并，本次未写入（可撤销回打开时的内容再保存）";

struct NotesPlugin {
    model: Mutex<NoteModel>,
    /// 热文档唯一，切走的文档留冷缓冲：flush / 关闭按会话身份找到并提交。
    buffers: Mutex<crate::session::DocBuffers>,
    ui: AsyncMutex<FrameUi>,
    /// 只读面本帧各行顶边（面内坐标）：双击空白时据此挑最近的行。
    rows: ui::RowCache,
    /// 双击空白进入编辑时控件层会按「点空」清掉焦点：事件处理完按活动输入面复位。
    refocus: Mutex<bool>,
    /// 当前文档的协作状态（正文增量与在场光标）；本地仓库下不产生任何上行。
    collab: Mutex<collab::Collab>,
}

impl NotesPlugin {
    fn new() -> Self {
        Self {
            model: Mutex::new(NoteModel::default()),
            buffers: Mutex::new(crate::session::DocBuffers::default()),
            ui: AsyncMutex::new(FrameUi::new()),
            rows: Arc::new(Mutex::new(Vec::new())),
            refocus: Mutex::new(false),
            collab: Mutex::new(collab::Collab::new()),
        }
    }

    async fn render(&self, ctx: &Context) {
        let mut snapshot = self.model.lock().unwrap().clone_for_view();
        let collab_active = self.collab.lock().unwrap().is_active();
        let mut frame_ui = self.ui.lock().await;
        // 在场保活与超时剔除按拍推进：没有协作文档时不要求帧拍
        frame_ui.set_fps(collab_active.then_some(COLLAB_TICK_HZ));
        snapshot.props.anchor = props::input_anchor(&mut frame_ui, &snapshot);
        if let Err(error) = frame_ui.render(ctx, PANEL, |frame| ui::frame(frame, &snapshot, &self.rows)).await {
            ctx.log_error(&format!("提交笔记面失败：{error}"));
            return;
        }
        // 刚进入槽位时输入框要到本帧才落位：再取一次光标矩形并补一帧，候选浮层当帧可见
        if snapshot.props.editing.is_some() && snapshot.props.anchor.is_none() {
            snapshot.props.anchor = props::input_anchor(&mut frame_ui, &snapshot);
            if snapshot.props.anchor.is_some() {
                if let Err(error) = frame_ui.render(ctx, PANEL, |frame| ui::frame(frame, &snapshot, &self.rows)).await {
                    ctx.log_error(&format!("提交笔记面失败：{error}"));
                }
            }
        }
    }

    /// 按会话身份读盘：宿主回执带 `open_seq`，迟到回执直接丢弃。
    async fn load(&self, ctx: &Context, doc_id: &str, open_seq: u64) {
        let mode = self.model.lock().unwrap().mode;
        match ctx.doc_open(doc_id, open_seq).await {
            Ok(result) => {
                // 读盘期间文档已切走：这份正文属于旧会话，不得落到新文档上
                if !self
                    .buffers
                    .lock()
                    .unwrap()
                    .is_current(&result.doc_id, result.open_seq)
                {
                    return;
                }
                {
                    let mut model = self.model.lock().unwrap();
                    let same = model.path.as_deref() == Some(result.path.as_str());
                    model.load(result.path.clone(), result.text.clone(), result.can_write);
                    if same {
                        // 同一篇改名后重开：档位不因换路径而回落
                        model.mode = mode;
                    }
                    model.doc.open(
                        crate::session::DocKey {
                            doc_id: result.doc_id.clone(),
                            open_seq: result.open_seq,
                        },
                        &result.text,
                        result.disk_rev,
                    );
                }
                self.buffers.lock().unwrap().prune(&result.path);
                // 正文到位后建立协作基线：换文件重建，同文件重开也下发一份新基线
                let (path, text, selection) = {
                    let model = self.model.lock().unwrap();
                    (model.path.clone().unwrap_or_default(), model.text.clone(), model.selection)
                };
                let out = self.collab.lock().unwrap().open(&path, &text, selection);
                self.send_collab(ctx, out).await;
                self.sync_cursors();
                self.load_backlinks(ctx, &result.path).await;
                self.refresh_tags(ctx).await;
                if self.model.lock().unwrap().wiki_files.is_empty() {
                    self.refresh_wiki_files(ctx).await;
                }
                self.refresh_images(ctx).await;
            }
            Err(error) => {
                ctx.log_error(&format!("打开笔记失败（{doc_id}）：{error}"));
                // 打开失败不得沿用上一篇正文（下一次编辑会被提交到新路径）。
                let mut model = self.model.lock().unwrap();
                model.clear();
                model.open_error = Some(error.to_string());
            }
        }
        self.refresh_prefs(ctx).await;
        self.render(ctx).await;
    }

    async fn refresh_prefs(&self, ctx: &Context) {
        match ctx.editor_prefs().await {
            Ok(prefs) => self.model.lock().unwrap().prefs = prefs,
            Err(error) => ctx.log_error(&format!("读取编辑偏好失败：{error}")),
        }
    }

    /// 全仓库标签词表（属性面板的标签候选）；空间模式由宿主聚合后随仓库变化重取。
    async fn refresh_tags(&self, ctx: &Context) {
        match ctx.vault_tags().await {
            Ok(tags) => self.model.lock().unwrap().tag_vocab = tags,
            Err(error) => ctx.log_error(&format!("读取标签词表失败：{error}")),
        }
    }

    /// 仓库文件清单（`[[` 候选与内链解析）；宿主清单重建时由变更事件重取。
    async fn refresh_wiki_files(&self, ctx: &Context) {
        match ctx.vault_files().await {
            Ok(result) => self.model.lock().unwrap().wiki_files = result.files,
            Err(error) => ctx.log_error(&format!("读取仓库文件清单失败：{error}")),
        }
    }

    /// 按输入值与光标刷新 `[[` 候选：离开链接范围、跨行或已有候选外内容即收起。
    /// 光标矩形在事件期算一次并存入模型，浮层绘制不必每帧重排正文。
    async fn refresh_wiki(&self, id: &str, value: &str, caret: usize) {
        let suggestion = {
            let model = self.model.lock().unwrap();
            if model.mode == Mode::Read || !model.can_write {
                None
            } else {
                crate::model::wiki_query(value, caret).and_then(|(start, query)| {
                    let candidates =
                        crate::model::wiki_candidates(&model.wiki_files, query, model.path.as_deref(), 6);
                    (!candidates.is_empty()).then(|| WikiSuggest {
                        input: id.to_string(),
                        start,
                        query: query.to_string(),
                        caret,
                        candidates,
                        caret_rect: None,
                    })
                })
            }
        };
        let Some(mut suggestion) = suggestion else {
            self.model.lock().unwrap().wiki = None;
            return;
        };
        suggestion.caret_rect = self.ui.lock().await.caret_rect_for(id, value, caret);
        self.model.lock().unwrap().wiki = Some(suggestion);
    }

    /// 链接点击：外链交系统浏览器，内链按仓库路径 / 当前目录 / 文件名解析后打开。
    async fn follow_link(&self, ctx: &Context, target: &str) {
        if crate::model::is_external(target) {
            if let Err(error) = ctx.open_url(target).await {
                ctx.log_error(&format!("打开外链失败（{target}）：{error}"));
                let _ = ctx
                    .notify("打开链接失败", Some(&error.to_string()), NotificationLevel::Error)
                    .await;
            }
            return;
        }
        let path = {
            let model = self.model.lock().unwrap();
            crate::model::resolve_link(&model.wiki_files, model.path.as_deref(), target)
        };
        match path {
            Some(path) => {
                if let Err(error) = ctx.open_file(&path).await {
                    ctx.log_error(&format!("打开内链失败（{path}）：{error}"));
                    let _ = ctx
                        .notify("打开链接失败", Some(&error.to_string()), NotificationLevel::Error)
                        .await;
                }
            }
            None => {
                let _ = ctx
                    .notify("找不到链接目标", Some(target), NotificationLevel::Warning)
                    .await;
            }
        }
    }

    /// 候选点击：把在输入的 `[[查询` 整体换成 `[[路径|文件名]]`，光标落在链接之后。
    async fn insert_wiki(&self, ctx: &Context, index: usize) {
        let Some((input, start, span, replacement)) = ({
            let model = self.model.lock().unwrap();
            let Some(wiki) = &model.wiki else { return };
            let Some(path) = wiki.candidates.get(index) else { return };
            Some((wiki.input.clone(), wiki.start, 2 + wiki.query.len(), crate::model::wiki_link(path)))
        }) else {
            return;
        };
        let (text, at) = {
            let mut model = self.model.lock().unwrap();
            if model.mode == Mode::Read || !model.can_write {
                return;
            }
            let base = input_base(&model, &input);
            let from = base + start;
            let to = from + span;
            if to > model.text.len() {
                return;
            }
            let mut text = model.text.clone();
            text.replace_range(from..to, &replacement);
            model.wiki = None;
            (text, from + replacement.len())
        };
        self.commit_edit(ctx, text, Some((at, at))).await;
        self.focus_at(ctx, at).await;
    }

    /// 探测正文引用的仓库图片能否解码，并记下原图像素尺寸：失败的在只读面回落替代文字，
    /// 尺寸供未声明宽高时的版面换算。结果按路径缓存，文件变更事件到达时由调用方失效重探。
    async fn refresh_images(&self, ctx: &Context) {
        let pending: Vec<String> = {
            let model = self.model.lock().unwrap();
            let mut out: Vec<String> = Vec::new();
            for block in &model.blocks {
                let Some(image) = &block.image else { continue };
                if image.src.starts_with("http://") || image.src.starts_with("https://") {
                    continue;
                }
                if model.image_probe.contains_key(&image.src) || out.contains(&image.src) {
                    continue;
                }
                out.push(image.src.clone());
            }
            out
        };
        for src in pending {
            let size = ctx.vault_image_info(&src).await.ok().map(|info| (info.width, info.height));
            self.model.lock().unwrap().image_probe.insert(src, size);
        }
    }

    async fn load_backlinks(&self, ctx: &Context, path: &str) {
        match ctx.vault_backlinks(path).await {
            Ok(entries) => {
                let rows = entries
                    .into_iter()
                    .map(|entry| Backlink { path: entry.path, title: entry.title })
                    .collect();
                self.model.lock().unwrap().backlinks = rows;
            }
            Err(error) => ctx.log_error(&format!("读取反链失败（{path}）：{error}")),
        }
    }

    /// 一次本地编辑：先写模型，再按会话身份把脏状态报给宿主（宿主只记账，不合并正文）。
    async fn commit_edit(&self, ctx: &Context, text: String, selection: Option<(usize, usize)>) {
        let (changed, key) = {
            let mut model = self.model.lock().unwrap();
            let changed = model.text != text;
            if changed {
                model.edit_text(text.clone());
            }
            model.selection = selection;
            (changed, model.doc.key().cloned())
        };
        if changed {
            let out = self.collab.lock().unwrap().edit(&text, selection);
            self.send_collab(ctx, out).await;
            self.sync_cursors();
        }
        if let Some(key) = key {
            self.report_state(ctx, &key).await;
        }
    }

    /// 把当前脏状态与保存错误报给宿主：宿主据此排自动保存并在标签栏显示胶囊。
    async fn report_state(&self, ctx: &Context, key: &crate::session::DocKey) {
        let (dirty, error) = {
            let model = self.model.lock().unwrap();
            (model.dirty, model.save_error.clone())
        };
        if let Err(e) = ctx.doc_state(&key.doc_id, key.open_seq, dirty, error.as_deref()).await {
            ctx.log_error(&format!("上报笔记会话状态失败（{}）：{e}", key.doc_id));
            // 宿主拒绝（超限 / 只读身份 / 会话已换）时必须可见：否则界面一直停在「未保存」。
            self.model.lock().unwrap().save_error = Some(e.to_string());
        }
    }

    /// 记一版历史：宿主不持正文，记版所需的全文只有本插件有。
    /// 给出合并窗口时窗口内的连续编辑并成一版；回滚按独立动作记版，不并入编辑窗口。
    async fn record_history(
        &self,
        ctx: &Context,
        path: &str,
        text: &str,
        action: &str,
        coalesce_ms: Option<i64>,
    ) {
        let params = HistoryRecordParams {
            kind: "note".to_string(),
            file: path.to_string(),
            content: text.to_string(),
            action: Some(action.to_string()),
            coalesce_ms,
            note: None,
        };
        if let Err(error) = ctx.history_record(&params).await {
            // 历史是侧车：失败只留可读原因，不把已完成的正文保存判成失败
            ctx.log_error(&format!("记录笔记历史失败（{path}）：{error}"));
        }
    }

    /// 冻结一次保存请求：有在途保存时按短间隔等它落地（同一文档不允许并发写）。
    /// 正文在真正冻结的那一刻取，等到的不是等待前的旧快照。
    /// 会话已关闭时返回 `None`（没有可提交的正文），与「等不到」同路收场。
    async fn begin_save_when_free(&self) -> Option<crate::session::SaveRequest> {
        for _ in 0..SAVE_BUSY_WAIT_ROUNDS {
            let (request, open) = {
                let mut model = self.model.lock().unwrap();
                let open = model.doc.key().is_some();
                let text = model.text.clone();
                (model.doc.begin_save(&text), open)
            };
            if request.is_some() || !open {
                return request;
            }
            tokio::time::sleep(SAVE_BUSY_WAIT).await;
        }
        None
    }

    /// 保存热文档：正文全文提交，宿主按 `expected_rev` 校验后原子写；冲突拿磁盘原文合流重试。
    async fn save_hot(&self, ctx: &Context, manual: bool) -> Result<(), String> {
        self.save_hot_as(ctx, manual, "edit", Some(HISTORY_COALESCE_MS)).await
    }

    /// 同上，落盘成功后按给定动作记版：回滚换掉的整篇正文不与编辑窗口合并。
    async fn save_hot_as(
        &self,
        ctx: &Context,
        manual: bool,
        action: &str,
        coalesce_ms: Option<i64>,
    ) -> Result<(), String> {
        for _ in 0..SAVE_ATTEMPTS {
            let key = {
                let model = self.model.lock().unwrap();
                if !model.can_write || !model.dirty {
                    return Ok(());
                }
                let Some(key) = model.doc.key().cloned() else { return Ok(()) };
                key
            };
            // 已有在途保存：等它落地再冻结本次正文；等不到就如实失败，不能谎报成功
            // （谎报会让宿主清掉脏标记，这一次的正文失去落盘目标）
            let Some(request) = self.begin_save_when_free().await else {
                self.model.lock().unwrap().save_error = Some(SAVE_BUSY.to_string());
                self.report_state(ctx, &key).await;
                return Err(SAVE_BUSY.to_string());
            };
            let result = match ctx
                .doc_save(
                    &key.doc_id,
                    key.open_seq,
                    &request.expected_rev,
                    &request.text,
                    manual,
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    // 宿主调用失败没有回执：撤回在途请求，正文保持脏并可见原因
                    let message = error.to_string();
                    {
                        let mut model = self.model.lock().unwrap();
                        model.doc.abort_save(&request);
                        model.dirty = true;
                        model.save_error = Some(message.clone());
                    }
                    self.report_state(ctx, &key).await;
                    return Err(message);
                }
            };
            // 会话在写盘期间被切走：在途请求只留在该文档的冷缓冲快照里，
            // 不撤回它就再也提交不了；保存没写成，脏状态必须报回去
            let stale = {
                let mut buffers = self.buffers.lock().unwrap();
                if buffers.is_current(&key.doc_id, key.open_seq) {
                    None
                } else {
                    let snapshot = buffers.cold(&key).cloned();
                    if let Some(mut doc) = snapshot.clone() {
                        doc.state.abort_save(&request);
                        doc.dirty = true;
                        buffers.store_cold(key.clone(), doc);
                    }
                    Some(snapshot.is_some())
                }
            };
            if let Some(report) = stale {
                if report {
                    self.report_cold_state(ctx, &key, true, None).await;
                }
                return Err("文档会话已切换，本次保存作废".to_string());
            }
            let mut recorded = None;
            let outcome = {
                let mut model = self.model.lock().unwrap();
                let current = model.text.clone();
                let outcome = model.doc.finish(&request, &current, result);
                match &outcome {
                    crate::session::SaveOutcome::Clean => {
                        model.dirty = false;
                        model.save_error = None;
                        // 落盘成功：磁盘内容成为三方合并的新共同祖先
                        if let Some(path) = model.path.clone() {
                            self.collab.lock().unwrap().flushed(&path, &request.text);
                            recorded = Some((path, request.text.clone()));
                        }
                    }
                    crate::session::SaveOutcome::Dirty => {
                        model.dirty = true;
                        model.save_error = None;
                    }
                    crate::session::SaveOutcome::Merged { text } => {
                        model.replace_text(text.clone());
                        model.dirty = true;
                        model.save_error = None;
                        // 合流结果含对端已落盘内容：静默对齐，不得当成本端新增广播出去
                        self.collab.lock().unwrap().aligned(text);
                    }
                    crate::session::SaveOutcome::Failed(message) => {
                        model.dirty = true;
                        model.save_error = Some(message.clone());
                    }
                    // 磁盘改动与本端改动无法安全合并：正文保持脏，磁盘原样不动
                    crate::session::SaveOutcome::Conflict => {
                        model.dirty = true;
                        model.save_error = Some(SAVE_CONFLICT.to_string());
                    }
                    crate::session::SaveOutcome::Stale => {}
                }
                outcome
            };
            match outcome {
                crate::session::SaveOutcome::Clean | crate::session::SaveOutcome::Stale => {
                    self.report_state(ctx, &key).await;
                    if let Some((path, text)) = recorded {
                        self.record_history(ctx, &path, &text, action, coalesce_ms).await;
                    }
                    return Ok(());
                }
                crate::session::SaveOutcome::Dirty
                | crate::session::SaveOutcome::Merged { .. } => continue,
                crate::session::SaveOutcome::Failed(message) => {
                    self.report_state(ctx, &key).await;
                    return Err(message);
                }
                crate::session::SaveOutcome::Conflict => {
                    self.report_state(ctx, &key).await;
                    return Err(SAVE_CONFLICT.to_string());
                }
            }
        }
        let message = "磁盘持续被外部修改，保存未完成".to_string();
        let key = {
            let mut model = self.model.lock().unwrap();
            model.save_error = Some(message.clone());
            model.doc.key().cloned()
        };
        if let Some(key) = key {
            self.report_state(ctx, &key).await;
        }
        Err(message)
    }

    /// 保存冷文档：切走但未关闭的文档按会话身份取冷缓冲提交。
    async fn save_cold(&self, ctx: &Context, key: crate::session::DocKey, manual: bool) -> Result<(), String> {
        for _ in 0..SAVE_ATTEMPTS {
            let Some(mut cold) = self.buffers.lock().unwrap().cold(&key).cloned() else {
                return Ok(());
            };
            if !cold.dirty || !cold.state.is_current(&key.doc_id, key.open_seq) {
                return Ok(());
            }
            let Some(request) = cold.state.begin_save(&cold.text) else {
                return Err("该文档已有在途保存，稍后重试".to_string());
            };
            let result = match ctx
                .doc_save(
                    &key.doc_id,
                    key.open_seq,
                    &request.expected_rev,
                    &request.text,
                    manual,
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    let message = error.to_string();
                    cold.state.abort_save(&request);
                    cold.dirty = true;
                    self.buffers.lock().unwrap().store_cold(key.clone(), cold);
                    self.report_cold_state(ctx, &key, true, Some(&message)).await;
                    return Err(message);
                }
            };
            let outcome = cold.state.finish(&request, &cold.text, result);
            match outcome {
                crate::session::SaveOutcome::Clean => {
                    cold.dirty = false;
                    self.buffers.lock().unwrap().store_cold(key.clone(), cold);
                    self.report_cold_state(ctx, &key, false, None).await;
                    // 冷文档只按普通编辑记版：整篇回滚的版本由发起回滚的那次保存另记
                    self.record_history(ctx, &key.doc_id, &request.text, "edit", Some(HISTORY_COALESCE_MS))
                        .await;
                    return Ok(());
                }
                crate::session::SaveOutcome::Dirty => {
                    cold.dirty = true;
                    self.buffers.lock().unwrap().store_cold(key.clone(), cold);
                }
                crate::session::SaveOutcome::Merged { text } => {
                    cold.text = text;
                    cold.dirty = true;
                    self.buffers.lock().unwrap().store_cold(key.clone(), cold);
                }
                crate::session::SaveOutcome::Failed(message) => {
                    cold.dirty = true;
                    self.buffers.lock().unwrap().store_cold(key.clone(), cold);
                    self.report_cold_state(ctx, &key, true, Some(&message)).await;
                    return Err(message);
                }
                crate::session::SaveOutcome::Conflict => {
                    cold.dirty = true;
                    self.buffers.lock().unwrap().store_cold(key.clone(), cold);
                    self.report_cold_state(ctx, &key, true, Some(SAVE_CONFLICT)).await;
                    return Err(SAVE_CONFLICT.to_string());
                }
                crate::session::SaveOutcome::Stale => return Ok(()),
            }
        }
        self.report_cold_state(ctx, &key, true, Some("磁盘持续被外部修改，保存未完成")).await;
        Err("磁盘持续被外部修改，保存未完成".to_string())
    }

    /// 冷文档的脏状态与保存错误上行（热文档走模型，冷文档只有缓冲里的状态）。
    async fn report_cold_state(
        &self,
        ctx: &Context,
        key: &crate::session::DocKey,
        dirty: bool,
        error: Option<&str>,
    ) {
        if let Err(e) = ctx.doc_state(&key.doc_id, key.open_seq, dirty, error).await {
            ctx.log_error(&format!("上报冷文档状态失败（{}）：{e}", key.doc_id));
        }
    }

    /// 宿主落盘请求：按会话身份提交热 / 冷文档正文。
    /// 身份已不在本端（插件重开过或宿主先结束了会话）即报错：谎报送 Ok 会让宿主判成落盘成功。
    async fn flush_document(&self, ctx: &Context, flush: &PanelFlushParams) -> Result<(), String> {
        let (Some(doc_id), Some(open_seq)) = (flush.doc_id.as_deref(), flush.open_seq) else {
            // 笔记的落盘一律按会话身份寻址：无身份请求无处可写，谎报成功会让宿主清掉脏标记
            return Err("落盘请求缺少文档会话身份".to_string());
        };
        let key = crate::session::DocKey { doc_id: doc_id.to_string(), open_seq };
        if self.buffers.lock().unwrap().is_current(&key.doc_id, key.open_seq) {
            return self.save_hot(ctx, false).await;
        }
        if self.buffers.lock().unwrap().cold(&key).is_some() {
            return self.save_cold(ctx, key, false).await;
        }
        Err("插件已不持有该文档会话的正文，无法落盘".to_string())
    }

    /// 属性读服务：只认当前热文档的会话身份。
    fn props_read_call(&self, params: &HostCallParams) -> Result<ContentPropsResult, String> {
        let request: ContentPropsReadParams = serde_json::from_value(params.params.clone())
            .map_err(|error| format!("属性读取参数不可解析：{error}"))?;
        self.props_session(&request.doc_id, request.open_seq)?;
        let model = self.model.lock().unwrap();
        let parsed = frontmatter::parse(&model.text);
        let closed = frontmatter::block_end(&model.text).is_some();
        Ok(ContentPropsResult {
            path: model.path.clone().unwrap_or_default(),
            fingerprint: text_fingerprint(&model.text),
            frontmatter: if closed { parsed.fm_prefix } else { String::new() },
            present: model.text.starts_with("---"),
            parsed: parsed.ok && closed,
            editable: model.can_write,
        })
    }

    /// 属性写服务：`value` 是属性面板交回的草稿（`frontmatter` + `fingerprint`），
    /// 按它重写全文并提交编辑协议，落盘仍走保存协议。
    async fn props_write_call(&self, ctx: &Context, params: &HostCallParams) -> Result<(), String> {
        let request: ContentPropsWriteParams = serde_json::from_value(params.params.clone())
            .map_err(|error| format!("属性写回参数不可解析：{error}"))?;
        self.props_session(&request.doc_id, request.open_seq)?;
        let draft = PropsDraft::parse(&request.value)?;
        let next = {
            let model = self.model.lock().unwrap();
            if !model.can_write {
                return Err("文档只读，属性未写入".to_string());
            }
            if draft.fingerprint != text_fingerprint(&model.text) {
                return Err("文档内容已变化，属性未写入".to_string());
            }
            let data = if draft.frontmatter.trim().is_empty() {
                Vec::new()
            } else {
                let parsed = frontmatter::parse(&draft.frontmatter);
                if !parsed.ok || frontmatter::block_end(&draft.frontmatter).is_none() {
                    return Err("属性块无法解析".to_string());
                }
                parsed
                    .data
                    .as_map()
                    .map(<[(String, Yaml)]>::to_vec)
                    .unwrap_or_default()
            };
            let next = model.text_with_props(data);
            (next != model.text).then_some(next)
        };
        if let Some(next) = next {
            self.commit_edit(ctx, next, None).await;
        }
        Ok(())
    }

    /// 属性转发的会话校验：会话已关闭或序号过期时拒绝，不把属性写到别的文档上。
    fn props_session(&self, doc_id: &str, open_seq: u64) -> Result<(), String> {
        if self.model.lock().unwrap().doc.is_current(doc_id, open_seq) {
            Ok(())
        } else {
            Err("文档会话已失效，请重新打开".to_string())
        }
    }

    async fn undo(&self, ctx: &Context, redo: bool) {
        let (text, selection, input) = {
            let mut model = self.model.lock().unwrap();
            let Some((text, selection)) = (if redo { model.redo() } else { model.undo() }) else {
                return;
            };
            let input = active_input(&model).and_then(|id| {
                let value = value_of(&model, &id)?;
                let base = input_base(&model, &id);
                let caret = selection
                    .map_or(value.len(), |(anchor, _)| anchor)
                    .saturating_sub(base)
                    .min(value.len());
                Some((id, value, caret))
            });
            (text, selection, input)
        };
        if let Some((id, value, caret)) = input {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.set_input(&id, &value, caret);
        }
        // 快照里的选区随撤销恢复：继续撤销的光标落点与上报给协作的选区都按它
        self.commit_edit(ctx, text, selection).await;
        self.render(ctx).await;
    }

    async fn handle_input(&self, ctx: &Context, params: FrameInputParams) {
        let input = params.input;
        // 点出面板即退出编辑：面板内工具条 / 属性 / 反链的编辑不受影响。
        // 失焦事件仍要交给控件层，否则属性槽 / 标题草稿不会提交、输入法组合串留在帧里。
        if matches!(input, FrameInput::Focus { focused: false }) {
            let mut model = self.model.lock().unwrap();
            if model.mode == Mode::Edit && model.can_write {
                model.mode = Mode::Read;
                model.active_block = None;
                model.active_item = None;
                model.reveal_pending = None;
            }
        }
        self.remember_pointer(&input);
        if let FrameInput::DoubleClick { hit, .. } = &input {
            self.on_pointer_double(ctx, hit.as_deref()).await;
        }
        if let FrameInput::PointerDown { hit, .. } = &input {
            self.on_pointer_down(ctx, hit.as_deref()).await;
        }
        if let FrameInput::PointerUp { .. } = &input {
            self.on_pointer_up(ctx).await;
        }
        let editable = {
            let model = self.model.lock().unwrap();
            model.mode != Mode::Read && model.can_write
        };
        // 标题 / 属性槽是控件层自管撤销的输入框：Ctrl+Z 交给它们，不回退整篇正文
        let in_field = {
            let frame_ui = self.ui.lock().await;
            frame_ui
                .focus_id()
                .is_some_and(|id| id == props::KEY_INPUT || id == crate::model::TITLE_INPUT)
        };
        if editable && !in_field {
            if let Some(redo) = undo_key(&input) {
                self.undo(ctx, redo).await;
                return;
            }
        }
        if save_key(&input) {
            // 未改动不写盘：空跑一次保存会多出一条历史版本。
            if self.model.lock().unwrap().dirty {
                let _ = self.save_hot(ctx, true).await;
                self.render(ctx).await;
            }
            return;
        }
        if self.handle_editor_key(ctx, &input).await {
            return;
        }
        let (events, carets) = {
            let values = self.model.lock().unwrap().clone_for_view();
            let mut frame_ui = self.ui.lock().await;
            let refocus = std::mem::take(&mut *self.refocus.lock().unwrap());
            let events = frame_ui.event(ctx, &input, |id| value_of(&values, id)).await;
            if refocus {
                // 双击处理已换过档位与活动输入面：按最新模型复位，旧快照找不到新输入面
                let model = self.model.lock().unwrap();
                refocus_active(&mut frame_ui, &model);
            }
            // 文本事件不带光标：在同一个输入状态机里取走（事件之后、释放锁之前）。
            let carets: Vec<(String, Option<(usize, usize)>)> = events
                .iter()
                .filter_map(|event| match event {
                    UiEvent::Text { id, .. } => Some((id.clone(), frame_ui.caret(id))),
                    _ => None,
                })
                .collect();
            (events, carets)
        };
        let mut carets = carets.into_iter();
        for event in events {
            match event {
                UiEvent::Click { id } => self.on_click(ctx, &id).await,
                UiEvent::DoubleClick { id } => self.on_double_click(ctx, &id).await,
                UiEvent::Text { id, value } => {
                    let caret = carets.next().and_then(|(_, caret)| caret);
                    self.on_text(ctx, &id, value, caret).await;
                }
                UiEvent::Submit { id } => self.on_submit(ctx, &id).await,
                UiEvent::Cancel { id } => self.on_cancel(&id).await,
                UiEvent::Focus { id, focused: false } => self.on_blur(ctx, &id).await,
                UiEvent::Caret { id, caret, anchor } => {
                    let selection = selection_of(&self.model.lock().unwrap(), &id, caret, anchor);
                    self.model.lock().unwrap().selection = selection;
                    let text = self.model.lock().unwrap().text.clone();
                    let out = self.collab.lock().unwrap().caret(&text, selection);
                    self.send_collab(ctx, out).await;
                    self.sync_cursors();
                    if caret == anchor {
                        let value = {
                            let model = self.model.lock().unwrap();
                            value_of(&model, &id)
                        };
                        if let Some(value) = value {
                            self.refresh_wiki(&id, &value, caret).await;
                        }
                    } else {
                        self.model.lock().unwrap().wiki = None;
                    }
                }
                _ => {}
            }
        }
        self.render(ctx).await;
    }

    fn remember_pointer(&self, input: &FrameInput) {
        let point = match input {
            FrameInput::PointerDown { x, y, .. }
            | FrameInput::PointerUp { x, y, .. }
            | FrameInput::DoubleClick { x, y, .. } => Some((*x, *y)),
            _ => None,
        };
        if let Some(point) = point {
            self.model.lock().unwrap().pointer = point;
        }
    }

    /// 预览编辑 / 只读档的正文按下：揭示块并把光标落到输入面（块粒度）。
    async fn on_pointer_down(&self, ctx: &Context, hit: Option<&str>) {
        let Some(hit) = hit else { return };
        if hit == "note:menu-backdrop" {
            self.model.lock().unwrap().menu_open = false;
            return;
        }
        {
            // 点候选行时保留候选（抬起后还要按行插入）；点输入框自身保留；点别处收起
            let mut model = self.model.lock().unwrap();
            let keep = model
                .wiki
                .as_ref()
                .is_some_and(|wiki| hit == wiki.input || hit.starts_with("note:wiki:"));
            if !keep {
                model.wiki = None;
            }
        }
        // 点面内其它位置先提交属性 / 标题草稿（控件层的按钮与正文不产生失焦事件）
        if self.model.lock().unwrap().props.editing.is_some() && !hit.starts_with("props:") {
            self.commit_props(ctx, 0).await;
        }
        if self.model.lock().unwrap().renaming.is_some() && hit != crate::model::TITLE_INPUT {
            self.submit_rename(ctx).await;
        }
        if !hit.starts_with("props:typepick:") {
            self.model.lock().unwrap().props.type_menu = None;
        }
        let mode = self.model.lock().unwrap().mode;
        if mode != Mode::Edit {
            return;
        }
        // 按下只记待揭示：立即揭示会把 markdown 块换成输入面，按住拖动的跨块选区随之销毁。
        // 抬起时位移小于阈值才真正揭示（见 on_pointer_up）。
        if item_index(hit).is_some() || block_index(hit).is_some() {
            let mut model = self.model.lock().unwrap();
            let already = item_index(hit)
                .is_some_and(|(block, item)| model.active_item == Some((block, item)))
                || block_index(hit).is_some_and(|index| {
                    model.active_block == Some(index)
                        && model.blocks.get(index).is_some_and(|block| block.items.is_empty())
                });
            if !already {
                model.reveal_pending = Some((hit.to_string(), model.pointer));
            }
            return;
        }
        if gap_input(hit).is_some() {
            // 空行自己托管光标：不揭示相邻块，交给控件层把光标落到空行上
            let mut model = self.model.lock().unwrap();
            model.active_block = None;
            model.active_item = None;
        }
    }

    /// 抬起结算待揭示单元：位移超过阈值 = 拖跨块选区，不揭示（选区留在控件层）。
    async fn on_pointer_up(&self, ctx: &Context) {
        let (hit, from) = match self.model.lock().unwrap().reveal_pending.take() {
            Some(pending) => pending,
            None => return,
        };
        let (x, y) = self.model.lock().unwrap().pointer;
        if (x - from.0).abs().max((y - from.1).abs()) >= REVEAL_DRAG_THRESHOLD {
            return;
        }
        if let Some((block, item)) = item_index(&hit) {
            let active = {
                let mut model = self.model.lock().unwrap();
                if model.active_item == Some((block, item)) {
                    return;
                }
                model.active_item = Some((block, item));
                model.active_block = None;
                true
            };
            if active {
                self.focus_item(ctx, block, item, None).await;
            }
            return;
        }
        if let Some(index) = block_index(&hit) {
            let active = {
                let mut model = self.model.lock().unwrap();
                // 有揭示单元的块（列表 / 引用）只按单元揭示：块 id 不整块回显源码
                if !model.blocks.get(index).is_some_and(|block| block.items.is_empty()) {
                    return;
                }
                if model.active_block == Some(index) {
                    return;
                }
                model.active_block = Some(index);
                model.active_item = None;
                true
            };
            if active {
                self.focus_block(ctx, index, None).await;
            }
        }
    }

    /// 只读档双击正文：进入预览编辑并揭示落点（块、揭示单元或空行）。
    /// 命中为空白时按指针纵向位置挑最近的行；光标统一由编辑面按落点换算。
    async fn on_pointer_double(&self, ctx: &Context, hit: Option<&str>) {
        {
            let model = self.model.lock().unwrap();
            if model.mode != Mode::Read || !model.can_write || model.blocks.is_empty() {
                return;
            }
        }
        let placeholder = hit.is_none();
        // 任务框的命中 id 是 `<项 id>::task:<序>`：揭示落在整项上
        let hit = hit.map(|hit| hit.split_once("::task:").map_or(hit, |(id, _)| id));
        let target = match hit {
            Some(hit)
                if item_index(hit).is_some()
                    || item_gap_index(hit).is_some()
                    || gap_input(hit).is_some()
                    || block_index(hit).is_some() =>
            {
                hit.to_string()
            }
            // 空白 / 容器命中：按指针位置挑最近的行（空行自己带命中，不参与兜底）
            _ => {
                let y = self.model.lock().unwrap().pointer.1;
                let rows = self.rows.lock().unwrap();
                let Some(id) = nearest_row(&rows, y) else { return };
                id
            }
        };
        // 「点空」的按下会被控件层解释成失去焦点：事件处理完由 refocus 把焦点还给编辑面
        if placeholder {
            *self.refocus.lock().unwrap() = true;
        }
        if let Some((index, item)) = item_index(&target) {
            {
                let mut model = self.model.lock().unwrap();
                model.mode = Mode::Edit;
                model.active_block = None;
                model.active_item = Some((index, item));
            }
            self.focus_item(ctx, index, item, None).await;
            return;
        }
        if let Some((block, item, line)) = item_gap_index(&target) {
            let value = {
                let model = self.model.lock().unwrap();
                let Some((from, to)) = model.item_gap_line_range(block, item, line) else { return };
                model.text[from..to].to_string()
            };
            {
                let mut model = self.model.lock().unwrap();
                model.mode = Mode::Edit;
                model.active_block = None;
                model.active_item = None;
            }
            let id = ui::item_gap_id(block, item, line);
            {
                let mut frame_ui = self.ui.lock().await;
                frame_ui.focus_input(&id, &value);
                frame_ui.set_input(&id, &value, 0);
            }
            self.render(ctx).await;
            return;
        }
        if let Some((gap, line)) = gap_input(&target) {
            {
                let mut model = self.model.lock().unwrap();
                model.mode = Mode::Edit;
                model.active_block = None;
                model.active_item = None;
            }
            // 空行自己托管光标：不揭示相邻块，交给编辑面把光标落到该行
            self.focus_gap(ctx, gap, line, 0).await;
            self.render(ctx).await;
            return;
        }
        let Some(index) = block_index(&target) else { return };
        {
            let mut model = self.model.lock().unwrap();
            if model.blocks.get(index).is_none() {
                return;
            }
            model.mode = Mode::Edit;
            model.active_block = Some(index);
            model.active_item = None;
        }
        self.focus_block(ctx, index, None).await;
    }

    /// 聚焦一个块输入面：`caret` 缺省按指针在块内的落点换算，`usize::MAX` = 块内容尾
    /// （源码区间还含块后空行，不能取 `value.len()`）；有揭示单元的块转到首单元。
    async fn focus_block(&self, ctx: &Context, index: usize, caret: Option<usize>) {
        let item = {
            let model = self.model.lock().unwrap();
            let Some(block) = model.blocks.get(index) else { return };
            if block.items.is_empty() {
                None
            } else {
                Some(if matches!(caret, Some(usize::MAX)) { block.items.len() - 1 } else { 0 })
            }
        };
        if let Some(item) = item {
            {
                let mut model = self.model.lock().unwrap();
                model.active_block = None;
                model.active_item = Some((index, item));
            }
            return self.focus_item(ctx, index, item, caret).await;
        }
        let (value, content_end) = {
            let model = self.model.lock().unwrap();
            let Some(value) = model.active_source().map(str::to_string) else { return };
            let end = model
                .blocks
                .get(index)
                .map(|block| block.to.saturating_sub(block.from))
                .unwrap_or(value.len());
            (value, end)
        };
        let id = ui::block_id(index);
        let pointer_caret = caret.is_none();
        let caret = match caret {
            Some(usize::MAX) => content_end,
            Some(caret) => caret.min(content_end),
            None => content_end,
        };
        let mut frame_ui = self.ui.lock().await;
        frame_ui.focus_input(&id, &value);
        frame_ui.set_input(&id, &value, caret);
        drop(frame_ui);
        self.render(ctx).await;
        if pointer_caret {
            self.set_caret_from_pointer(&id, &value).await;
        }
    }

    /// 聚焦一个列表项输入面：`caret` 缺省按指针在项内的落点换算，`usize::MAX` = 项尾。
    async fn focus_item(&self, ctx: &Context, block: usize, item: usize, caret: Option<usize>) {
        let (value, caret, pointer_caret) = {
            let model = self.model.lock().unwrap();
            let Some((from, to)) = model.item_source(block, item) else { return };
            let value = model.text[from..to].to_string();
            let pointer_caret = caret.is_none();
            let caret = match caret {
                Some(usize::MAX) => value.len(),
                Some(caret) => caret.min(value.len()),
                None => value.len(),
            };
            (value, caret, pointer_caret)
        };
        let id = ui::item_id(block, item);
        {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.focus_input(&id, &value);
            frame_ui.set_input(&id, &value, caret);
        }
        self.render(ctx).await;
        if pointer_caret {
            self.set_caret_from_pointer(&id, &value).await;
        }
    }

    /// 输入面当帧已渲染：按指针落点换算字符位（与编辑面点击换算同口径）。
    async fn set_caret_from_pointer(&self, id: &str, value: &str) {
        let (px, py) = {
            let model = self.model.lock().unwrap();
            model.pointer
        };
        let mut frame_ui = self.ui.lock().await;
        let Some(caret) = caret_at_pointer(&mut frame_ui, id, value, px, py) else { return };
        frame_ui.set_input(id, value, caret);
    }

    /// 聚焦一根空行：光标落在空行内容里，不揭示任何块。
    async fn focus_gap(&self, _ctx: &Context, gap: usize, line: usize, caret: usize) {
        let (value, caret) = {
            let model = self.model.lock().unwrap();
            let Some((from, to)) = model.gap_line_range(gap, line) else { return };
            let value = model.text[from..to].to_string();
            let caret = caret.min(value.len());
            (value, caret)
        };
        let id = ui::gap_input_id(gap, line);
        let mut frame_ui = self.ui.lock().await;
        frame_ui.focus_input(&id, &value);
        frame_ui.set_input(&id, &value, caret);
    }

    /// 聚焦列表项之间的一根空行：光标落在空行内容里，不揭示任何块与项。
    async fn focus_item_gap(&self, block: usize, item: usize, line: usize, caret: usize) {
        let (value, caret) = {
            let model = self.model.lock().unwrap();
            let Some((from, to)) = model.item_gap_line_range(block, item, line) else { return };
            let value = model.text[from..to].to_string();
            let caret = caret.min(value.len());
            (value, caret)
        };
        let id = ui::item_gap_id(block, item, line);
        let mut frame_ui = self.ui.lock().await;
        frame_ui.focus_input(&id, &value);
        frame_ui.set_input(&id, &value, caret);
    }

    /// 正文编辑后把光标落回新的结构：优先列表项，其次块，再次空行，最后落到末块末尾。
    async fn focus_at(&self, _ctx: &Context, offset: usize) -> bool {
        let target = {
            let mut model = self.model.lock().unwrap();
            if let Some((block, item)) = model.item_at(offset) {
                let Some((from, to)) = model.item_source(block, item) else { return false };
                let value = model.text[from..to].to_string();
                model.active_block = None;
                model.active_item = Some((block, item));
                let caret = offset.saturating_sub(from).min(value.len());
                (ui::item_id(block, item), value, caret)
            } else if let Some((block, item, line)) = model.item_gap_line_at(offset) {
                // 项间空行自己托管光标：不并到相邻项，否则每敲一个字符焦点都被挪走
                let Some((from, to)) = model.item_gap_line_range(block, item, line) else {
                    return false;
                };
                let value = model.text[from..to].to_string();
                model.active_block = None;
                model.active_item = None;
                let caret = offset.saturating_sub(from).min(value.len());
                (ui::item_gap_id(block, item, line), value, caret)
            } else if let Some(index) = model.block_at(offset) {
                if !model.blocks[index].items.is_empty() {
                    // 有揭示单元的块内但落在单元间空行：落到最近的单元（前项末尾 / 首项开头）
                    let item = model.blocks[index]
                        .items
                        .iter()
                        .rposition(|info| model.body_start + info.to <= offset)
                        .unwrap_or(0);
                    let Some((from, to)) = model.item_source(index, item) else { return false };
                    let value = model.text[from..to].to_string();
                    model.active_block = None;
                    model.active_item = Some((index, item));
                    let caret = offset.saturating_sub(from).min(value.len());
                    (ui::item_id(index, item), value, caret)
                } else {
                    let Some((from, _)) = model.block_source(index) else { return false };
                    let Some(value) = model.active_source().map(str::to_string) else { return false };
                    model.active_block = Some(index);
                    model.active_item = None;
                    let caret = offset.saturating_sub(from).min(value.len());
                    (ui::block_id(index), value, caret)
                }
            } else if let Some((gap, line)) = model.gap_line_at(offset) {
                let Some((from, to)) = model.gap_line_range(gap, line) else { return false };
                let value = model.text[from..to].to_string();
                model.active_block = None;
                model.active_item = None;
                let caret = offset.saturating_sub(from).min(value.len());
                (ui::gap_input_id(gap, line), value, caret)
            } else {
                let Some(index) = model.blocks.len().checked_sub(1) else { return false };
                if !model.blocks[index].items.is_empty() {
                    let item = model.blocks[index].items.len() - 1;
                    let Some((from, to)) = model.item_source(index, item) else { return false };
                    let value = model.text[from..to].to_string();
                    model.active_block = None;
                    model.active_item = Some((index, item));
                    let caret = offset.min(to).saturating_sub(from).min(value.len());
                    (ui::item_id(index, item), value, caret)
                } else {
                    let Some((from, to)) = model.block_source(index) else { return false };
                    let Some(value) = model.active_source().map(str::to_string) else { return false };
                    model.active_block = Some(index);
                    model.active_item = None;
                    let caret = offset.min(to).saturating_sub(from).min(value.len());
                    (ui::block_id(index), value, caret)
                }
            }
        };
        let (id, value, caret) = target;
        let mut frame_ui = self.ui.lock().await;
        frame_ui.focus_input(&id, &value);
        frame_ui.set_input(&id, &value, caret);
        true
    }

    /// 编辑档的语义按键：属性槽步进 / 空行退格与纵向巡航 / 块内列表续项与缩进。
    async fn handle_editor_key(&self, ctx: &Context, input: &FrameInput) -> bool {
        use atelyx_plugin_protocol::frame::NamedKey;
        let FrameInput::Key { key, action, modifiers, .. } = input else { return false };
        if *action != KeyAction::Press || modifiers.control || modifiers.alt || modifiers.meta {
            return false;
        }
        let focus = {
            let frame_ui = self.ui.lock().await;
            frame_ui.focus_id().map(str::to_string)
        };
        let Some(focus) = focus else { return false };
        if focus == crate::model::TITLE_INPUT {
            if matches!(key, Key::Named { name: NamedKey::Enter }) {
                self.submit_rename(ctx).await;
                return true;
            }
            return false;
        }
        if focus == props::KEY_INPUT {
            return self.handle_props_key(ctx, key, modifiers.shift).await;
        }
        if focus == SOURCE {
            return self.handle_source_key(ctx, key, modifiers.shift).await;
        }
        if let Some((gap, line)) = gap_input(&focus) {
            return self.handle_gap_key(ctx, &focus, gap, line, key).await;
        }
        if let Some((block, item, line)) = item_gap_index(&focus) {
            return self.handle_item_gap_key(ctx, &focus, block, item, line, key).await;
        }
        if let Some((block, item)) = item_index(&focus) {
            return self.handle_item_key(ctx, &focus, block, item, key, modifiers.shift).await;
        }
        let active = {
            let model = self.model.lock().unwrap();
            if model.mode != Mode::Edit || !model.can_write {
                return false;
            }
            model.active_block
        };
        let Some(index) = active else { return false };
        let id = ui::block_id(index);
        if focus != id {
            return false;
        }
        let (caret, anchor) = {
            let frame_ui = self.ui.lock().await;
            match frame_ui.caret(&id) {
                Some(caret) => caret,
                None => return false,
            }
        };
        let (from, to) = if anchor <= caret { (anchor, caret) } else { (caret, anchor) };
        let Some(source) = self.model.lock().unwrap().active_source().map(str::to_string) else {
            return false;
        };
        // 块输入面首行 / 末行的 ↑↓ 跨出输入面：先进相邻空行，再进相邻块（有选区时不跨）
        if let Key::Named { name: NamedKey::ArrowUp | NamedKey::ArrowDown } = key {
            let up = matches!(key, Key::Named { name: NamedKey::ArrowUp });
            let cross = from == to && if up { caret == 0 } else { caret == source.len() };
            if cross {
                let target = {
                    let model = self.model.lock().unwrap();
                    if up {
                        let lines = model.gap_lines(index).len();
                        if lines > 0 {
                            Some(Focus::Gap(index, lines - 1, usize::MAX))
                        } else {
                            index.checked_sub(1).map(|prev| Focus::Block(prev, usize::MAX))
                        }
                    } else if index + 1 < model.blocks.len() {
                        Some(Focus::Block(index + 1, 0))
                    } else {
                        None
                    }
                };
                if let Some(target) = target {
                    self.move_to(ctx, target).await;
                    return true;
                }
            }
        }
        let change = match key {
            Key::Named { name: NamedKey::Enter } if from == to => {
                edit::list_enter_edit(&source, caret).map(|(text, at)| (text, at, at))
            }
            Key::Named { name: NamedKey::Backspace } if from == to => {
                edit::list_backspace_edit(&source, caret).map(|(text, at)| (text, at, at))
            }
            Key::Named { name: NamedKey::Tab } if modifiers.shift => {
                match edit::outdent_edit(&source, from, to) {
                    Some(edit) => Some(selection_of_edit(edit, anchor <= caret)),
                    // 无可删缩进也吞掉按键：交给控件层会把焦点挪走
                    None => return true,
                }
            }
            Key::Named { name: NamedKey::Tab } => {
                Some(selection_of_edit(edit::indent_edit(&source, from, to), anchor <= caret))
            }
            _ => None,
        };
        let Some((source, sel_anchor, sel_caret)) = change else { return false };
        let (text, base) = {
            let model = self.model.lock().unwrap();
            let Some((base, _)) = model.block_source(index) else { return false };
            let Some(text) = model.replace_block(index, &source) else { return false };
            (text, base)
        };
        {
            let mut frame_ui = self.ui.lock().await;
            // 置光标会折叠选区：视觉落点取光标端，选区两端仍按实际更新上报给协作
            frame_ui.set_input(&id, &source, sel_caret);
        }
        self.commit_edit(ctx, text, Some((base + sel_anchor, base + sel_caret))).await;
        self.render(ctx).await;
        true
    }

    /// 属性槽里的按键：↑↓ 巡航候选、Enter / Tab 提交并步进（Tab 反向步进）。
    async fn handle_props_key(&self, ctx: &Context, key: &Key, shift: bool) -> bool {
        use atelyx_plugin_protocol::frame::NamedKey;
        match key {
            Key::Named { name: NamedKey::ArrowDown } | Key::Named { name: NamedKey::ArrowUp } => {
                let delta = if matches!(key, Key::Named { name: NamedKey::ArrowUp }) { -1 } else { 1 };
                let (moved, offset) = {
                    let mut model = self.model.lock().unwrap();
                    let moved = props::move_suggestion(&mut *model, delta);
                    (moved, props::suggest_scroll_offset(&*model))
                };
                if moved {
                    self.ui.lock().await.scroll_to(props::SUGGEST_SCROLL, offset);
                }
                true
            }
            Key::Named { name: NamedKey::Enter } => {
                self.submit_props(ctx).await;
                true
            }
            Key::Named { name: NamedKey::Tab } => {
                self.commit_props(ctx, if shift { -1 } else { 1 }).await;
                true
            }
            _ => false,
        }
    }

    /// 空行输入面的按键：退格合并上一行、↑↓ 在空行与相邻块之间巡航；Tab 不参与控件巡航。
    async fn handle_gap_key(&self, ctx: &Context, id: &str, gap: usize, line: usize, key: &Key) -> bool {
        use atelyx_plugin_protocol::frame::NamedKey;
        let editable = {
            let model = self.model.lock().unwrap();
            model.mode == Mode::Edit && model.can_write
        };
        if !editable {
            return false;
        }
        match key {
            Key::Named { name: NamedKey::Tab } => true,
            Key::Named { name: NamedKey::Backspace } => {
                let (caret, anchor) = {
                    let frame_ui = self.ui.lock().await;
                    frame_ui.caret(id).unwrap_or((0, 0))
                };
                if caret != 0 || anchor != 0 {
                    return false;
                }
                let Some(offset) = self.model.lock().unwrap().gap_lines(gap).get(line).copied() else {
                    return false;
                };
                let Some((text, at)) = self.model.lock().unwrap().join_gap_line(offset) else {
                    return false;
                };
                self.commit_edit(ctx, text, Some((at, at))).await;
                self.focus_at(ctx, at).await;
                true
            }
            Key::Named { name: NamedKey::Delete } => {
                let (caret, anchor) = {
                    let frame_ui = self.ui.lock().await;
                    frame_ui.caret(id).unwrap_or((0, 0))
                };
                let (value_len, end) = {
                    let model = self.model.lock().unwrap();
                    match model.gap_line_range(gap, line) {
                        Some((from, to)) => (to - from, to),
                        None => return false,
                    }
                };
                if caret != value_len || anchor != caret {
                    return false;
                }
                let Some((text, at)) = self.model.lock().unwrap().remove_newline(end) else {
                    return false;
                };
                self.commit_edit(ctx, text, Some((at, at))).await;
                self.focus_at(ctx, at).await;
                true
            }
            Key::Named { name: NamedKey::ArrowUp } => {
                self.move_gap_vertical(ctx, gap, line, -1).await;
                true
            }
            Key::Named { name: NamedKey::ArrowDown } => {
                self.move_gap_vertical(ctx, gap, line, 1).await;
                true
            }
            _ => false,
        }
    }

    /// 列表项间空行输入面的按键：退格并入上一行、Delete 并入下一行，
    /// ↑↓ 在空行与相邻项之间巡航；Tab 不参与控件巡航。
    async fn handle_item_gap_key(
        &self,
        ctx: &Context,
        id: &str,
        block: usize,
        item: usize,
        line: usize,
        key: &Key,
    ) -> bool {
        use atelyx_plugin_protocol::frame::NamedKey;
        let editable = {
            let model = self.model.lock().unwrap();
            model.mode == Mode::Edit && model.can_write
        };
        if !editable {
            return false;
        }
        match key {
            Key::Named { name: NamedKey::Tab } => true,
            Key::Named { name: NamedKey::Backspace } => {
                let (caret, anchor) = {
                    let frame_ui = self.ui.lock().await;
                    frame_ui.caret(id).unwrap_or((0, 0))
                };
                if caret != 0 || anchor != 0 {
                    return false;
                }
                let Some(offset) = self
                    .model
                    .lock()
                    .unwrap()
                    .item_gap_line_range(block, item, line)
                    .map(|(from, _)| from)
                else {
                    return false;
                };
                let Some((text, at)) = self.model.lock().unwrap().join_gap_line(offset) else {
                    return false;
                };
                self.commit_edit(ctx, text, Some((at, at))).await;
                self.focus_at(ctx, at).await;
                true
            }
            Key::Named { name: NamedKey::Delete } => {
                let (caret, anchor) = {
                    let frame_ui = self.ui.lock().await;
                    frame_ui.caret(id).unwrap_or((0, 0))
                };
                let (value_len, end) = {
                    let model = self.model.lock().unwrap();
                    match model.item_gap_line_range(block, item, line) {
                        Some((from, to)) => (to - from, to),
                        None => return false,
                    }
                };
                if caret != value_len || anchor != caret {
                    return false;
                }
                let Some((text, at)) = self.model.lock().unwrap().remove_newline(end) else {
                    return false;
                };
                self.commit_edit(ctx, text, Some((at, at))).await;
                self.focus_at(ctx, at).await;
                true
            }
            Key::Named { name: NamedKey::ArrowUp } | Key::Named { name: NamedKey::ArrowDown } => {
                let up = matches!(key, Key::Named { name: NamedKey::ArrowUp });
                let target = {
                    let model = self.model.lock().unwrap();
                    if up {
                        if line > 0 {
                            Some(Focus::ItemGap(block, item, line - 1, 0))
                        } else if item > 0 {
                            Some(Focus::Item(block, item - 1, usize::MAX))
                        } else if block > 0 {
                            Some(Focus::Block(block - 1, usize::MAX))
                        } else {
                            None
                        }
                    } else if line + 1 < model.item_gap_lines(block, item) {
                        Some(Focus::ItemGap(block, item, line + 1, 0))
                    } else if model.blocks.get(block).is_some_and(|info| item + 1 < info.items.len()) {
                        Some(Focus::Item(block, item + 1, 0))
                    } else if block + 1 < model.blocks.len() {
                        Some(Focus::Block(block + 1, 0))
                    } else {
                        None
                    }
                };
                if let Some(target) = target {
                    self.move_to(ctx, target).await;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// 源码档 Tab / Shift+Tab：整行缩进与反缩进。Tab 先于控件层消费，
    /// 否则会被控件层的焦点巡航吃掉（跳到工具条输入框）。
    async fn handle_source_key(&self, ctx: &Context, key: &Key, shift: bool) -> bool {
        use atelyx_plugin_protocol::frame::NamedKey;
        if !matches!(key, Key::Named { name: NamedKey::Tab }) {
            return false;
        }
        let (caret, anchor) = {
            let frame_ui = self.ui.lock().await;
            match frame_ui.caret(SOURCE) {
                Some(caret) => caret,
                None => return false,
            }
        };
        let (from, to) = if anchor <= caret { (anchor, caret) } else { (caret, anchor) };
        let (text, sel_anchor, sel_caret) = {
            let model = self.model.lock().unwrap();
            if model.mode != Mode::Source || !model.can_write {
                return false;
            }
            let edit = if shift {
                match edit::outdent_edit(&model.text, from, to) {
                    Some(edit) => edit,
                    // 无可删缩进也吞掉按键：交给控件层会把焦点挪走
                    None => return true,
                }
            } else {
                edit::indent_edit(&model.text, from, to)
            };
            selection_of_edit(edit, anchor <= caret)
        };
        self.commit_edit(ctx, text.clone(), Some((sel_anchor, sel_caret))).await;
        {
            let mut frame_ui = self.ui.lock().await;
            // 置光标会折叠选区：视觉落点取光标端，选区两端仍按实际更新上报给协作
            frame_ui.set_input(SOURCE, &text, sel_caret);
        }
        self.render(ctx).await;
        true
    }

    /// 列表项输入面的语义按键：续项 / 空项退格 / 缩进，↑↓ 在项与相邻单元之间巡航。
    async fn handle_item_key(
        &self,
        ctx: &Context,
        id: &str,
        block: usize,
        item: usize,
        key: &Key,
        shift: bool,
    ) -> bool {
        use atelyx_plugin_protocol::frame::NamedKey;
        let editable = {
            let model = self.model.lock().unwrap();
            model.mode == Mode::Edit && model.can_write
        };
        if !editable {
            return false;
        }
        let (caret, anchor) = {
            let frame_ui = self.ui.lock().await;
            match frame_ui.caret(id) {
                Some(caret) => caret,
                None => return false,
            }
        };
        let (from, to) = if anchor <= caret { (anchor, caret) } else { (caret, anchor) };
        let Some((unit_from, unit_to)) = self.model.lock().unwrap().item_source(block, item) else {
            return false;
        };
        let Some(source) = self.model.lock().unwrap().text.get(unit_from..unit_to).map(str::to_string) else {
            return false;
        };
        // 项首 / 项尾的 ↑↓ 跨出：先在列表内换项，再进相邻块（有选区时不跨）
        if let Key::Named { name: NamedKey::ArrowUp | NamedKey::ArrowDown } = key {
            let up = matches!(key, Key::Named { name: NamedKey::ArrowUp });
            let cross = from == to && if up { caret == 0 } else { caret == source.len() };
            if cross {
                let target = {
                    let model = self.model.lock().unwrap();
                    if up {
                        if item > 0 {
                            Some(Focus::Item(block, item - 1, usize::MAX))
                        } else if block > 0 {
                            Some(Focus::Block(block - 1, usize::MAX))
                        } else {
                            None
                        }
                    } else if model.blocks.get(block).is_some_and(|info| item + 1 < info.items.len()) {
                        Some(Focus::Item(block, item + 1, 0))
                    } else if block + 1 < model.blocks.len() {
                        Some(Focus::Block(block + 1, 0))
                    } else {
                        None
                    }
                };
                if let Some(target) = target {
                    self.move_to(ctx, target).await;
                    return true;
                }
            }
        }
        let change = match key {
            Key::Named { name: NamedKey::Enter } if from == to => {
                edit::list_enter_edit(&source, caret).map(|(text, at)| (text, at, at))
            }
            Key::Named { name: NamedKey::Backspace } if from == to => {
                edit::list_backspace_edit(&source, caret).map(|(text, at)| (text, at, at))
            }
            Key::Named { name: NamedKey::Tab } if shift => {
                match edit::outdent_edit(&source, from, to) {
                    Some(edit) => Some(selection_of_edit(edit, anchor <= caret)),
                    // 无可删缩进也吞掉按键：交给控件层会把焦点挪走
                    None => return true,
                }
            }
            Key::Named { name: NamedKey::Tab } => {
                Some(selection_of_edit(edit::indent_edit(&source, from, to), anchor <= caret))
            }
            _ => None,
        };
        let Some((source, sel_anchor, sel_caret)) = change else { return false };
        let text = {
            let model = self.model.lock().unwrap();
            let Some(text) = model.replace_item(block, item, &source) else { return false };
            text
        };
        // 置光标会折叠选区：落点取光标端，选区两端仍按实际更新上报给协作与撤销快照
        self.commit_edit(ctx, text, Some((unit_from + sel_anchor, unit_from + sel_caret))).await;
        self.focus_at(ctx, unit_from + sel_caret).await;
        self.render(ctx).await;
        true
    }

    /// 纵向巡航到另一个输入面（块 / 列表项 / 空行）。
    async fn move_to(&self, ctx: &Context, target: Focus) {
        match target {
            Focus::Block(index, caret) => {
                {
                    let mut model = self.model.lock().unwrap();
                    model.active_block = Some(index);
                    model.active_item = None;
                }
                self.focus_block(ctx, index, Some(caret)).await;
            }
            Focus::Item(block, item, caret) => {
                {
                    let mut model = self.model.lock().unwrap();
                    model.active_block = None;
                    model.active_item = Some((block, item));
                }
                self.focus_item(ctx, block, item, Some(caret)).await;
            }
            Focus::Gap(gap, line, caret) => {
                {
                    let mut model = self.model.lock().unwrap();
                    model.active_block = None;
                    model.active_item = None;
                }
                self.focus_gap(ctx, gap, line, caret).await;
                self.render(ctx).await;
            }
            Focus::ItemGap(block, item, line, caret) => {
                {
                    let mut model = self.model.lock().unwrap();
                    model.active_block = None;
                    model.active_item = None;
                }
                self.focus_item_gap(block, item, line, caret).await;
                self.render(ctx).await;
            }
        }
    }

    /// 空行输入面里的 ↑↓：先在同一空行区内换行，越界后进相邻块（上一块末尾 / 下一块开头）。
    async fn move_gap_vertical(&self, ctx: &Context, gap: usize, line: usize, dir: i32) {
        let target = {
            let model = self.model.lock().unwrap();
            let lines = model.gap_lines(gap).len();
            let next_line = line as i32 + dir;
            if next_line >= 0 && (next_line as usize) < lines {
                Some(Focus::Gap(gap, next_line as usize, 0))
            } else if dir < 0 {
                gap.checked_sub(1).and_then(|block| {
                    model.block_source(block).map(|_| Focus::Block(block, usize::MAX))
                })
            } else if gap < model.blocks.len() {
                Some(Focus::Block(gap, 0))
            } else {
                None
            }
        };
        let Some(target) = target else { return };
        self.move_to(ctx, target).await;
    }

    async fn on_click(&self, ctx: &Context, id: &str) {
        // 先取链接目标再释放 ui 锁：持锁跨 await 会挡住同一面的渲染
        let link = self.ui.lock().await.link_target(id);
        if let Some(target) = link {
            self.follow_link(ctx, &target).await;
            return;
        }
        if self.props_click(ctx, id).await {
            return;
        }
        // 任务框命中：`<项 id>::task:<序>`，整项只画一个框
        if let Some((item_id, _)) = id.split_once("::task:") {
            if let Some((block, item)) = item_index(item_id) {
                self.toggle_task(ctx, block, item).await;
                return;
            }
        }
        match id {
            "note:retry" => {
                self.model.lock().unwrap().save_error = None;
                let _ = self.save_hot(ctx, true).await;
            }
            "note:error-dismiss" => self.model.lock().unwrap().save_error = None,
            "note:menu" => {
                let mut model = self.model.lock().unwrap();
                model.menu_open = !model.menu_open;
                model.props.type_menu = None;
            }
            "note:menu-backdrop" => self.model.lock().unwrap().menu_open = false,
            "note:menu:history" => {
                self.model.lock().unwrap().menu_open = false;
                let path = self.model.lock().unwrap().path.clone();
                if let Some(path) = path {
                    if let Err(error) = ctx.history_open("note", &path).await {
                        ctx.log_error(&format!("打开历史失败（{path}）：{error}"));
                    }
                }
            }
            "note:menu:add-prop" => {
                self.model.lock().unwrap().menu_open = false;
                self.add_property(ctx).await;
            }
            "note:mode:read" | "note:mode:edit" | "note:mode:source" => {
                let mode = match id {
                    "note:mode:read" => Mode::Read,
                    "note:mode:edit" => Mode::Edit,
                    _ => Mode::Source,
                };
                let mut model = self.model.lock().unwrap();
                if mode != Mode::Read && !model.can_write {
                    return;
                }
                model.mode = mode;
                model.active_block = None;
                model.active_item = None;
                model.props.type_menu = None;
                model.menu_open = false;
                model.wiki = None;
                if mode == Mode::Source {
                    model.props.editing = None;
                    model.props.draft.clear();
                    props::reset_suggest(&mut *model);
                }
            }
            crate::model::TITLE => {
                {
                    let mut model = self.model.lock().unwrap();
                    if !model.can_write {
                        return;
                    }
                    model.renaming = Some(model.title());
                }
                self.focus_title_input().await;
            }
            _ if id.starts_with("note:wiki:") => {
                let Ok(index) = id["note:wiki:".len()..].parse::<usize>() else { return };
                self.insert_wiki(ctx, index).await;
            }
            _ => {
                let Some(index) = id.strip_prefix("note:backlink:") else { return };
                let Ok(index) = index.parse::<usize>() else { return };
                let path = self
                    .model
                    .lock()
                    .unwrap()
                    .backlinks
                    .get(index)
                    .map(|link| link.path.clone());
                if let Some(path) = path {
                    if let Err(error) = ctx.open_file(&path).await {
                        ctx.log_error(&format!("打开反链文件失败（{path}）：{error}"));
                        let _ = ctx
                            .notify("打开文件失败", Some(&error.to_string()), NotificationLevel::Error)
                            .await;
                    }
                }
            }
        }
    }

    async fn on_double_click(&self, ctx: &Context, id: &str) {
        // 任务框命中 id 附带 `::task:<序>`，揭示按所属单元算
        let id = id.split_once("::task:").map_or(id, |(id, _)| id);
        let mode = self.model.lock().unwrap().mode;
        if mode == Mode::Read {
            self.on_pointer_double(ctx, Some(id)).await;
            return;
        }
        if mode != Mode::Edit {
            return;
        }
        if let Some((block, item)) = item_index(id) {
            {
                let model = self.model.lock().unwrap();
                // 已在编辑本单元：双击只做选词（控件层已处理），不重置光标
                if model.active_item == Some((block, item)) {
                    return;
                }
            }
            {
                let mut model = self.model.lock().unwrap();
                model.active_block = None;
                model.active_item = Some((block, item));
            }
            self.focus_item(ctx, block, item, None).await;
            return;
        }
        let Some(index) = block_index(id) else { return };
        {
            let model = self.model.lock().unwrap();
            // 有揭示单元的块（列表 / 引用）只按单元揭示，块 id 双击不整块回显源码
            if !model.blocks.get(index).is_some_and(|block| block.items.is_empty()) {
                return;
            }
            if model.active_block == Some(index) {
                // 已在编辑本块：双击只做选词（控件层已处理），不重置光标
                return;
            }
        }
        {
            let mut model = self.model.lock().unwrap();
            model.active_block = Some(index);
            model.active_item = None;
        }
        self.focus_block(ctx, index, None).await;
    }

    /// 任务项点击：翻转勾选态并把光标落在勾选框内（协作在场不跳到文末）。
    /// 显示档位不作权限：能否写由 `can_write` 决定，被宿主拒绝时回填保存错误。
    async fn toggle_task(&self, ctx: &Context, block: usize, item: usize) {
        let (next, at) = {
            let model = self.model.lock().unwrap();
            if !model.can_write {
                return;
            }
            let Some((from, _)) = model.item_source(block, item) else { return };
            let Some(toggled) = edit::task_toggle_edit(&model.text, from) else { return };
            toggled
        };
        self.commit_edit(ctx, next, Some((at, at))).await;
    }

    /// 属性面板的点击：命中即处理并返回 true。
    async fn props_click(&self, ctx: &Context, id: &str) -> bool {
        if id.starts_with("props:typepick:") {
            let text = props::on_type_pick(&mut *self.model.lock().unwrap(), id);
            if let Some(text) = text {
                self.commit_edit(ctx, text, None).await;
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("props:suggest:") {
            let Ok(index) = rest.parse::<usize>() else { return true };
            let (text, next) = {
                let mut model = self.model.lock().unwrap();
                props::pick_suggestion(&mut *model, index)
            };
            if let Some(text) = text {
                self.commit_edit(ctx, text, None).await;
            }
            if next {
                self.focus_props_input().await;
            }
            return true;
        }
        if !id.starts_with("props:") {
            {
                // 点别处收起类型菜单
                let mut model = self.model.lock().unwrap();
                if model.props.type_menu.is_some() {
                    model.props.type_menu = None;
                }
            }
            return false;
        }
        let quick = id.starts_with("props:delete:")
            || id.starts_with("props:delete-item:")
            || id.starts_with("props:toggle-bool:");
        let (editing, empty_key) = {
            let model = self.model.lock().unwrap();
            let empty_key = matches!(model.props.editing, Some(crate::model::Slot::Key(_)))
                && model.props.draft.trim().is_empty();
            (model.props.editing.is_some(), empty_key)
        };
        if editing {
            if quick || empty_key {
                // 删除 / 翻转按渲染下标执行；空键名提交会删行，也先丢草稿，避免下标漂移
                props::cancel_edit(&mut *self.model.lock().unwrap());
            } else {
                self.commit_props(ctx, 0).await;
            }
        }
        if id == "props:add" {
            self.add_property(ctx).await;
            return true;
        }
        if quick {
            return self.props_quick_action(ctx, id).await;
        }
        let focus = props::on_click(&mut *self.model.lock().unwrap(), id);
        if focus {
            self.focus_props_input().await;
        }
        true
    }

    async fn props_quick_action(&self, ctx: &Context, id: &str) -> bool {
        let text = {
            let mut model = self.model.lock().unwrap();
            props::quick_action(&mut *model, id)
        };
        if let Some(text) = text {
            self.commit_edit(ctx, text, None).await;
        }
        true
    }

    /// Enter：高亮候选优先，否则提交并步进。
    async fn submit_props(&self, ctx: &Context) {
        let (text, next) = {
            let mut model = self.model.lock().unwrap();
            props::submit(&mut *model, 1)
        };
        if let Some(text) = text {
            self.commit_edit(ctx, text, None).await;
        }
        if next {
            self.focus_props_input().await;
        }
    }

    async fn commit_props(&self, ctx: &Context, dir: i32) {
        let (text, next) = {
            let mut model = self.model.lock().unwrap();
            props::commit(&mut *model, dir)
        };
        if let Some(text) = text {
            self.commit_edit(ctx, text, None).await;
        }
        if next {
            self.focus_props_input().await;
        }
    }

    /// 追加一条空属性：占位键立即写进正文（Esc 后仍留在文件里），随后进入键名编辑。
    async fn add_property(&self, ctx: &Context) {
        let (added, text) = {
            let mut model = self.model.lock().unwrap();
            if props::add_property(&mut *model) {
                (true, props::text_with_placeholder(&mut *model))
            } else {
                (false, None)
            }
        };
        if let Some(text) = text {
            self.commit_edit(ctx, text, None).await;
        }
        if added {
            self.focus_props_input().await;
        }
    }

    async fn focus_props_input(&self) {
        let (draft, editing) = {
            let model = self.model.lock().unwrap();
            (model.props.draft.clone(), model.props.editing.is_some())
        };
        if !editing {
            return;
        }
        let mut frame_ui = self.ui.lock().await;
        frame_ui.focus_input(props::KEY_INPUT, &draft);
        frame_ui.select_all_input(props::KEY_INPUT, &draft);
        frame_ui.scroll_to(props::SUGGEST_SCROLL, 0.0);
    }

    async fn focus_title_input(&self) {
        let draft = self.model.lock().unwrap().renaming.clone().unwrap_or_default();
        let mut frame_ui = self.ui.lock().await;
        frame_ui.focus_input(crate::model::TITLE_INPUT, &draft);
        frame_ui.set_input(crate::model::TITLE_INPUT, &draft, draft.len());
    }

    async fn on_submit(&self, ctx: &Context, id: &str) {
        if id == props::KEY_INPUT {
            self.commit_props(ctx, 1).await;
        } else if id == crate::model::TITLE_INPUT {
            self.submit_rename(ctx).await;
        }
    }

    async fn on_cancel(&self, id: &str) {
        {
            let mut model = self.model.lock().unwrap();
            if model.wiki.as_ref().is_some_and(|wiki| wiki.input == id) {
                model.wiki = None;
            }
        }
        if id == props::KEY_INPUT {
            let mut model = self.model.lock().unwrap();
            model.props.editing = None;
            model.props.draft.clear();
            model.props.error = None;
            props::reset_suggest(&mut *model);
            // 丢弃未提交的草稿；已写进正文的占位属性保留
            model.refresh_props();
        } else if id == crate::model::TITLE_INPUT {
            self.model.lock().unwrap().renaming = None;
        }
    }

    async fn on_blur(&self, ctx: &Context, id: &str) {
        {
            let mut model = self.model.lock().unwrap();
            if model.wiki.as_ref().is_some_and(|wiki| wiki.input == id) {
                model.wiki = None;
            }
        }
        if id == props::KEY_INPUT {
            self.commit_props(ctx, 0).await;
        } else if id == crate::model::TITLE_INPUT {
            self.submit_rename(ctx).await;
        }
    }

    /// 标题改名：有未落盘修改先保存，保存回执到达后再发起改名。
    async fn submit_rename(&self, ctx: &Context) {
        let (path, name, dirty) = {
            let mut model = self.model.lock().unwrap();
            let Some(draft) = model.renaming.take() else { return };
            let Some(path) = model.path.clone() else { return };
            (path, draft.trim().to_string(), model.dirty)
        };
        if name.is_empty() {
            return;
        }
        if !self.model.lock().unwrap().can_write {
            return;
        }
        if name == self.model.lock().unwrap().title() {
            return;
        }
        if dirty {
            self.model.lock().unwrap().pending_rename = Some(name);
            let _ = self.save_hot(ctx, false).await;
            return;
        }
        if let Err(error) = ctx.doc_rename(&path, &name).await {
            ctx.log_error(&format!("改名失败（{path}）：{error}"));
            let _ = ctx.notify("改名失败", Some(&error.to_string()), NotificationLevel::Error).await;
        }
    }

    async fn on_text(&self, ctx: &Context, id: &str, value: String, caret: Option<(usize, usize)>) {
        if id == props::KEY_INPUT {
            {
                let mut model = self.model.lock().unwrap();
                model.props.suggest_query = value.clone();
                model.props.draft = value;
                model.props.suggest_index = 0;
                model.props.suggest_armed = false;
            }
            self.ui.lock().await.scroll_to(props::SUGGEST_SCROLL, 0.0);
            return;
        }
        if id == crate::model::TITLE_INPUT {
            // 文件名是单行：粘贴 / 换行一律压成空格
            self.model.lock().unwrap().renaming = Some(value.replace(['\r', '\n'], " "));
            return;
        }
        let value = edit::normalize_newlines(value);
        let range = {
            let model = self.model.lock().unwrap();
            if let Some((gap, line)) = gap_input(id) {
                model.gap_line_range(gap, line)
            } else if let Some((block, item, line)) = item_gap_index(id) {
                model.item_gap_line_range(block, item, line)
            } else {
                item_index(id).and_then(|(block, item)| model.item_source(block, item))
            }
        };
        if let Some((from, to)) = range {
            let (text, at) = {
                let model = self.model.lock().unwrap();
                if model.mode != Mode::Edit || !model.can_write {
                    return;
                }
                let caret = caret.map_or(value.len(), |(caret, _)| caret);
                let (text, at) = model.replace_range(from, to, &value, caret);
                (text, at)
            };
            self.refresh_wiki(id, &value, caret.map_or(value.len(), |(caret, _)| caret)).await;
            self.commit_edit(ctx, text, Some((at, at))).await;
            self.focus_at(ctx, at).await;
            return;
        }
        let value_len = value.len();
        let (text, selection, handoff) = {
            let model = self.model.lock().unwrap();
            if model.mode == Mode::Read || !model.can_write {
                return;
            }
            let next = if id == SOURCE {
                value.clone()
            } else if id == crate::model::EMPTY_EDIT {
                let body_start = model.body_start.min(model.text.len());
                let mut out = String::with_capacity(body_start + value.len());
                out.push_str(&model.text[..body_start]);
                out.push_str(&value);
                out
            } else {
                let Some(index) = block_index(id) else { return };
                let Some(next) = model.replace_block(index, &value) else { return };
                next
            };
            let (caret, anchor) = caret.unwrap_or((value_len, value_len));
            let selection = selection_of(&model, id, caret, anchor);
            // 空笔记敲出第一个块后，编辑面 id 从 `EMPTY_EDIT` 换成块 0：把焦点一并交给它，
            // 否则下一次渲染输入框被 prune，后续按键无处可去。
            let handoff = (id == crate::model::EMPTY_EDIT && !model.blocks.is_empty())
                .then(|| {
                    let block = ui::block_id(0);
                    value_of(&model, &block).map(|value| (block, value, caret))
                })
                .flatten();
            (next, selection, handoff)
        };
        // 候选跟活动输入面走：空笔记敲出首块后，输入面已换成块 0
        let caret = caret.map_or((value_len, value_len), |pair| pair).0;
        if let Some((id, value, caret)) = &handoff {
            self.refresh_wiki(id, value, *caret).await;
        } else {
            self.refresh_wiki(id, &value, caret).await;
        }
        if let Some((id, value, caret)) = handoff {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.focus_input(&id, &value);
            frame_ui.set_input(&id, &value, caret);
        }
        self.commit_edit(ctx, text, selection).await;
    }
}

impl NoteModel {
    /// 渲染闭包只读快照：避免渲染期间与输入处理争锁。
    /// 撤销栈与保存状态不参与渲染——保存状态含上次落盘正文与在途请求全文，每帧复制代价过高。
    fn clone_for_view(&self) -> NoteModel {
        NoteModel {
            undo: Vec::new(),
            redo: Vec::new(),
            doc: crate::session::DocState::default(),
            ..self.clone()
        }
    }
}

fn active_input(model: &NoteModel) -> Option<String> {
    match model.mode {
        Mode::Source => Some(SOURCE.to_string()),
        Mode::Edit => match model.active_item {
            Some((block, item)) => Some(ui::item_id(block, item)),
            None => match model.active_block {
                Some(index) => Some(ui::block_id(index)),
                None if model.blocks.is_empty() => Some(crate::model::EMPTY_EDIT.to_string()),
                None => None,
            },
        },
        Mode::Read => None,
    }
}

fn block_index(id: &str) -> Option<usize> {
    id.strip_prefix("note:block:")?.parse().ok()
}

/// 空行输入面 id →（空行区序，行序）。
fn gap_input(id: &str) -> Option<(usize, usize)> {
    let rest = id.strip_prefix("note:gap:")?;
    let (gap, line) = rest.split_once(':')?;
    Some((gap.parse().ok()?, line.parse().ok()?))
}

/// 列表项输入面 id →（块序，项序）。
fn item_index(id: &str) -> Option<(usize, usize)> {
    let rest = id.strip_prefix("note:item:")?;
    let (block, item) = rest.split_once(':')?;
    Some((block.parse().ok()?, item.parse().ok()?))
}

/// 列表项间空行输入面 id →（块序，后一项序，行序）。
fn item_gap_index(id: &str) -> Option<(usize, usize, usize)> {
    let rest = id.strip_prefix("note:igap:")?;
    let (block, rest) = rest.split_once(':')?;
    let (item, line) = rest.split_once(':')?;
    Some((block.parse().ok()?, item.parse().ok()?, line.parse().ok()?))
}

/// 空白双击挑最近的行：跳过空行（它们自己带命中区），取行顶不高于点击 y 的最后一行，
/// 都低不过时取最上面一行；没有可编辑行时为 None。
fn nearest_row(rows: &[(String, f32)], y: f32) -> Option<String> {
    let mut best: Option<(&str, f32)> = None;
    let mut first: Option<(&str, f32)> = None;
    for (id, top) in rows {
        if id.starts_with("note:gap:") || id.starts_with("note:igap:") {
            continue;
        }
        if first.map_or(true, |(_, first_top)| *top < first_top) {
            first = Some((id, *top));
        }
        if *top <= y && best.map_or(true, |(_, best_top)| *top > best_top) {
            best = Some((id, *top));
        }
    }
    best.or(first).map(|(id, _)| id.to_string())
}

/// 输入面当帧已渲染：按光标矩形在 (行, 横坐标) 序上二分，求指针处最近字符位。
fn caret_at_pointer(frame_ui: &mut FrameUi, id: &str, value: &str, px: f32, py: f32) -> Option<usize> {
    let offsets: Vec<usize> = value.char_indices().map(|(at, _)| at).chain(std::iter::once(value.len())).collect();
    let (mut lo, mut hi) = (0usize, offsets.len() - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let rect = frame_ui.caret_rect_for(id, value, offsets[mid])?;
        // 光标行在点击下方，或同一行但在点击右侧 = 该光标位在落点之后
        let after = rect.y > py || (py >= rect.y && py < rect.y + rect.h && rect.x > px);
        if after {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(offsets[lo])
}

/// 双击空白进入编辑后控件层按「点空」清焦点：事件处理完把焦点还给活动输入面。
fn refocus_active(frame_ui: &mut FrameUi, model: &NoteModel) {
    let Some(id) = active_input(model) else { return };
    let Some(value) = value_of(model, &id) else { return };
    let caret = frame_ui.caret(&id).map_or(value.len(), |(caret, _)| caret);
    frame_ui.focus_input(&id, &value);
    frame_ui.set_input(&id, &value, caret);
}

/// 纵向巡航的目标（↑↓ 跨输入面时用）。
enum Focus {
    /// 块序号 + 光标（`usize::MAX` = 块尾）。
    Block(usize, usize),
    /// 揭示单元：块序 + 单元序 + 光标（`usize::MAX` = 单元尾）。
    Item(usize, usize, usize),
    /// 空行区序 + 行序 + 光标。
    Gap(usize, usize, usize),
    /// 列表项间空行：块序 + 后一项序 + 行序 + 光标。
    ItemGap(usize, usize, usize, usize),
}

/// 输入框文本在全文中的起点（选区偏移按它换算成框内光标）。
fn input_base(model: &NoteModel, id: &str) -> usize {
    if id == crate::model::EMPTY_EDIT {
        return model.body_start;
    }
    if let Some((block, item, line)) = item_gap_index(id) {
        return model.item_gap_line_range(block, item, line).map_or(0, |(from, _)| from);
    }
    if let Some((block, item)) = item_index(id) {
        return model.item_source(block, item).map_or(0, |(from, _)| from);
    }
    if let Some(index) = block_index(id) {
        return model
            .blocks
            .get(index)
            .map_or(0, |block| model.body_start + block.from);
    }
    0
}

fn value_of(model: &NoteModel, id: &str) -> Option<String> {
    if id == SOURCE {
        return Some(model.text.clone());
    }
    if id == crate::model::EMPTY_EDIT {
        return Some(model.body().to_string());
    }
    if let Some((gap, line)) = gap_input(id) {
        let (from, to) = model.gap_line_range(gap, line)?;
        return Some(model.text[from..to].to_string());
    }
    if let Some((block, item, line)) = item_gap_index(id) {
        let (from, to) = model.item_gap_line_range(block, item, line)?;
        return Some(model.text[from..to].to_string());
    }
    if let Some((block, item)) = item_index(id) {
        let (from, to) = model.item_source(block, item)?;
        return Some(model.text[from..to].to_string());
    }
    let index = block_index(id)?;
    let (from, to) = model.block_source(index)?;
    Some(model.text[from..to].to_string())
}

/// 缩进 / 反缩进后的文本与选区两端，按原来的选择方向还原锚点与光标端。
fn selection_of_edit(edit: edit::LineEdit, forward: bool) -> (String, usize, usize) {
    if forward {
        (edit.text, edit.anchor, edit.caret)
    } else {
        (edit.text, edit.caret, edit.anchor)
    }
}

/// 输入面内相对光标 → 全文绝对选区。
fn selection_of(
    model: &NoteModel,
    id: &str,
    caret: usize,
    anchor: usize,
) -> Option<(usize, usize)> {
    if id == SOURCE {
        return Some((anchor.min(model.text.len()), caret.min(model.text.len())));
    }
    let base = if id == crate::model::EMPTY_EDIT {
        model.body_start.min(model.text.len())
    } else if let Some((gap, line)) = gap_input(id) {
        model.gap_line_range(gap, line)?.0
    } else if let Some((block, item, line)) = item_gap_index(id) {
        model.item_gap_line_range(block, item, line)?.0
    } else if let Some((block, item)) = item_index(id) {
        model.item_source(block, item)?.0
    } else {
        model.block_source(block_index(id)?)?.0
    };
    let value_len = value_of(model, id).map(|value| value.len()).unwrap_or(0);
    let clamp = |offset: usize| base + offset.min(value_len);
    Some((clamp(anchor), clamp(caret)))
}

/// 宿主通知是否属于插件当前热文档：带会话身份时按身份判定，缺身份时退回路径匹配。
fn doc_identity_matches(
    model: &NoteModel,
    doc_id: Option<&str>,
    open_seq: Option<u64>,
    path: Option<&str>,
) -> bool {
    match (doc_id, open_seq) {
        (Some(doc_id), Some(open_seq)) => model.doc.is_current(doc_id, open_seq),
        _ => match path {
            Some(path) => model.path.as_deref() == Some(path),
            None => false,
        },
    }
}

/// 属性面板交回的草稿：本插件 props.read 回执里的 frontmatter 与 fingerprint 原样带回。
/// value 的具体形状由内容所有者定义，宿主只转发。
struct PropsDraft {
    frontmatter: String,
    fingerprint: String,
}

impl PropsDraft {
    fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let field = |name: &str| {
            value
                .get(name)
                .and_then(|item| item.as_str())
                .unwrap_or_default()
                .to_string()
        };
        Ok(Self { frontmatter: field("frontmatter"), fingerprint: field("fingerprint") })
    }
}

/// 属性读写回执的内容指纹：同一进程内对当前正文取摘要，只用于拒绝把属性块写到已变的文档上。
fn text_fingerprint(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Ctrl/Cmd+S = 手动保存（面持有键盘焦点时宿主快捷键让位，由插件显式转发）。
fn save_key(input: &FrameInput) -> bool {
    let FrameInput::Key { key, action, modifiers, .. } = input else { return false };
    if *action != KeyAction::Press || !(modifiers.control || modifiers.meta) {
        return false;
    }
    matches!(key, Key::Character { text } if text.eq_ignore_ascii_case("s"))
}

/// Ctrl/Cmd+Z = 撤销；Ctrl/Cmd+Shift+Z 与 Ctrl+Y = 重做。命中即吞掉这一按键（不再交给控件层）。
fn undo_key(input: &FrameInput) -> Option<bool> {
    let FrameInput::Key { key, action, modifiers, .. } = input else { return None };
    if *action != KeyAction::Press {
        return None;
    }
    let Key::Character { text } = key else { return None };
    if !(modifiers.control || modifiers.meta) {
        return None;
    }
    if text.eq_ignore_ascii_case("z") {
        Some(modifiers.shift)
    } else if text.eq_ignore_ascii_case("y") {
        Some(true)
    } else {
        None
    }
}

#[async_trait::async_trait]
impl PluginHandler for NotesPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "笔记", Some("file-text"), None)
            .await
        {
            ctx.log_error(&format!("注册笔记面板失败：{error}"));
        }
        if let Err(error) = ctx
            .register_service(
                content_method::SERVICE,
                &[content_method::PROPS_READ, content_method::PROPS_WRITE],
            )
            .await
        {
            ctx.log_error(&format!("注册笔记属性服务失败：{error}"));
        }
        for name in [
            event::name::DOC_CHANGED,
            event::name::DOC_RESTORE,
            event::name::EDITOR_PREFS_CHANGED,
            event::name::VAULT_SWITCH,
            event::name::VAULT_FILE_CHANGED,
            event::name::VAULT_FILES_CHANGED,
            event::name::PANEL_FLUSH,
            event::name::SPACE_PEERS_CHANGED,
            event::name::SPACE_RESYNC,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        self.refresh_prefs(ctx).await;
        self.refresh_wiki_files(ctx).await;
        self.render(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != PANEL {
            return;
        }
        self.ui.lock().await.refresh_theme();
        self.refresh_prefs(ctx).await;
        self.render(ctx).await;
    }

    async fn on_doc_open(&self, ctx: &Context, params: DocInstanceOpenParams) {
        if params.surface_id != PANEL {
            return;
        }
        self.ui.lock().await.refresh_theme();
        let key = crate::session::DocKey {
            doc_id: params.doc_id.clone(),
            open_seq: params.open_seq,
        };
        // 旧热文档冻结进冷缓冲：切走后 flush / 关闭仍能按身份提交它
        let previous = {
            let model = self.model.lock().unwrap();
            let buffers = self.buffers.lock().unwrap();
            buffers.hot().filter(|_| model.path.is_some()).map(|old| {
                (
                    old.clone(),
                    crate::session::ColdDoc {
                        text: model.text.clone(),
                        dirty: model.dirty,
                        state: model.doc.clone(),
                    },
                )
            })
        };
        self.buffers.lock().unwrap().activate(key, previous);
        let same = self
            .model
            .lock()
            .unwrap()
            .path
            .as_deref()
            == Some(params.path.as_str());
        if !same {
            self.model.lock().unwrap().clear();
        }
        self.refresh_room(ctx).await;
        self.load(ctx, &params.doc_id, params.open_seq).await;
    }

    async fn on_doc_close(&self, ctx: &Context, params: DocInstanceCloseParams) {
        if params.surface_id != PANEL {
            return;
        }
        let key = crate::session::DocKey {
            doc_id: params.doc_id.clone(),
            open_seq: params.open_seq,
        };
        let hot = self
            .buffers
            .lock()
            .unwrap()
            .is_current(&params.doc_id, params.open_seq);
        if hot {
            // 先落盘再拆协作：写盘失败时文档还开着，协作通道不能被提前关掉
            if let Err(error) = self.save_hot(ctx, false).await {
                ctx.log_error(&format!("关闭前落盘失败（{}）：{error}", params.doc_id));
                self.render(ctx).await;
                return;
            }
            let out = self.collab.lock().unwrap().close();
            self.send_collab(ctx, out).await;
            if let Err(error) = ctx.doc_close(&params.doc_id, params.open_seq).await {
                ctx.log_error(&format!("关闭笔记失败（{}）：{error}", params.doc_id));
            }
            self.buffers.lock().unwrap().forget(&key);
            self.model.lock().unwrap().clear();
            // 关档后宿主仍持有上一帧：必须交一帧空态，否则面板停在旧文档
            self.render(ctx).await;
        } else if self.buffers.lock().unwrap().cold(&key).is_some() {
            // 冷文档被关：先提交冷缓冲正文，失败即中止（同热文档口径）
            if let Err(error) = self.save_cold(ctx, key.clone(), false).await {
                ctx.log_error(&format!("关闭前落盘失败（{}）：{error}", params.doc_id));
                return;
            }
            if let Err(error) = ctx.doc_close(&params.doc_id, params.open_seq).await {
                ctx.log_error(&format!("关闭笔记失败（{}）：{error}", params.doc_id));
            }
            self.buffers.lock().unwrap().forget(&key);
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        match params.name.as_str() {
            event::name::DOC_CHANGED => {
                let Ok(payload) = serde_json::from_value::<DocChangedPayload>(params.payload) else {
                    ctx.log_error("笔记状态载荷不可解析");
                    return;
                };
                {
                    let model = self.model.lock().unwrap();
                    // 会话身份不符即迟到通知：不得把旧文档的状态盖到新文档
                    if !doc_identity_matches(
                        &model,
                        payload.doc_id.as_deref(),
                        payload.open_seq,
                        Some(&payload.path),
                    ) {
                        return;
                    }
                }
                // 判脏方是本插件：只把宿主回执当作「可以重试改名」的信号，不反向采纳落盘状态
                let (pending, dirty, path) = {
                    let mut model = self.model.lock().unwrap();
                    let pending = if model.dirty { None } else { model.pending_rename.take() };
                    (pending, model.dirty, model.path.clone())
                };
                if let (Some(name), false) = (pending, dirty) {
                    if let Some(path) = path {
                        if let Err(error) = ctx.doc_rename(&path, &name).await {
                            ctx.log_error(&format!("改名失败（{path}）：{error}"));
                            let _ = ctx
                                .notify("改名失败", Some(&error.to_string()), NotificationLevel::Error)
                                .await;
                        }
                    }
                }
                self.refresh_images(ctx).await;
                self.render(ctx).await;
            }
            event::name::DOC_RESTORE => {
                let Ok(payload) = serde_json::from_value::<DocRestorePayload>(params.payload) else {
                    ctx.log_error("回滚载荷不可解析");
                    return;
                };
                let accepted = {
                    let mut model = self.model.lock().unwrap();
                    if !doc_identity_matches(&model, Some(&payload.doc_id), Some(payload.open_seq), None) {
                        return;
                    }
                    if model.text != payload.content {
                        model.replace_text(payload.content.clone());
                        // 整篇换文会让槽位下标失效：丢弃在途槽编辑，避免提交到别的属性上
                        model.props.editing = None;
                        model.props.draft.clear();
                        props::reset_suggest(&mut *model);
                        let restored = model.text.clone();
                        model.doc.adopt_text(&restored);
                        model.dirty = true;
                        true
                    } else {
                        false
                    }
                };
                if !accepted {
                    return;
                }
                // 协作文档同步到回滚后的正文：否则下一次差分会把回滚掉的内容当本端新增再发出去
                let text = self.model.lock().unwrap().text.clone();
                self.collab.lock().unwrap().aligned(&text);
                self.reset_inputs().await;
                if let Err(error) = self.save_hot_as(ctx, false, "restore", None).await {
                    ctx.log_error(&format!("回滚后落盘失败：{error}"));
                }
                self.render(ctx).await;
            }
            event::name::EDITOR_PREFS_CHANGED => {
                let Ok(prefs) = serde_json::from_value::<EditorPrefs>(params.payload) else {
                    ctx.log_error("编辑偏好载荷不可解析");
                    return;
                };
                self.model.lock().unwrap().prefs = prefs;
                self.render(ctx).await;
            }
            event::name::VAULT_SWITCH => {
                self.buffers.lock().unwrap().clear();
                self.model.lock().unwrap().clear();
                self.refresh_room(ctx).await;
                self.refresh_wiki_files(ctx).await;
                self.render(ctx).await;
            }
            event::name::SPACE_PEERS_CHANGED => {
                self.refresh_room(ctx).await;
                self.sync_cursors();
                self.render(ctx).await;
            }
            event::name::SPACE_RESYNC => {
                // 断连期间的帧已丢失：重发基线与握手，强制重发在场身份
                let out = self.collab.lock().unwrap().resync();
                self.send_collab(ctx, out).await;
            }
            event::name::PANEL_FLUSH => {
                let Ok(flush) = serde_json::from_value::<PanelFlushParams>(params.payload) else {
                    ctx.log_error("落盘请求载荷不可解析");
                    return;
                };
                let result = self.flush_document(ctx, &flush).await;
                let done = PanelFlushDoneParams {
                    target: Some(flush.target),
                    token: flush.token,
                    doc_id: flush.doc_id,
                    open_seq: flush.open_seq,
                    ok: result.is_ok(),
                    error: result.err(),
                };
                if let Err(error) = ctx.panel_flush_done(&done).await {
                    ctx.log_error(&format!("回报落盘结果失败：{error}"));
                }
                self.render(ctx).await;
            }
            event::name::VAULT_FILES_CHANGED => {
                self.refresh_wiki_files(ctx).await;
                // 候选浮层开着时按新清单重算；填不上就不出，继续输入会再触发
                let open = {
                    let model = self.model.lock().unwrap();
                    let Some(wiki) = &model.wiki else { return };
                    let Some(value) = value_of(&model, &wiki.input) else { return };
                    (wiki.input.clone(), value, wiki.caret)
                };
                self.refresh_wiki(&open.0, &open.1, open.2).await;
                self.render(ctx).await;
            }
            event::name::VAULT_FILE_CHANGED => {
                let Ok(payload) = serde_json::from_value::<FileChangedPayload>(params.payload) else {
                    ctx.log_error("仓库文件变更载荷不可解析");
                    return;
                };
                if matches!(payload.kind, FileChangeKind::Deleted) {
                    let path = self.model.lock().unwrap().path.clone();
                    if path.as_deref() == Some(payload.path.as_str()) {
                        let key = self.model.lock().unwrap().doc.key().cloned();
                        if let Some(key) = key {
                            self.buffers.lock().unwrap().forget(&key);
                        }
                        self.model.lock().unwrap().clear();
                        self.render(ctx).await;
                        return;
                    }
                }
                if matches!(payload.kind, FileChangeKind::Renamed) {
                    if let Some(old) = payload.old_path.as_deref() {
                        self.follow_rename(ctx, old, &payload.path).await;
                    }
                }
                // 探测结论按 markdown 原始 src 缓存，变更路径是仓库相对路径：同一条目按尾部匹配清掉
                self.model
                    .lock()
                    .unwrap()
                    .image_probe
                    .retain(|src, _| !crate::model::image_src_matches(src, &payload.path));
                let path = self.model.lock().unwrap().path.clone();
                if let Some(path) = path {
                    self.load_backlinks(ctx, &path).await;
                }
                self.refresh_images(ctx).await;
                self.render(ctx).await;
            }
            _ => {}
        }
    }

    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        let result = if params.service != content_method::SERVICE {
            Err(format!("未提供该服务：{}", params.service))
        } else {
            match params.method.as_str() {
                content_method::PROPS_READ => self
                    .props_read_call(&params)
                    .and_then(|result| {
                        serde_json::to_value(result)
                            .map_err(|error| format!("属性回执不可序列化：{error}"))
                    }),
                content_method::PROPS_WRITE => self
                    .props_write_call(ctx, &params)
                    .await
                    .map(|_| serde_json::json!({})),
                _ => Err(format!("未提供该服务方法：{}", params.method)),
            }
        };
        let response = match result {
            Ok(value) => ctx.service_result(&params.call_id, value).await,
            Err(error) => ctx.service_error(&params.call_id, &error).await,
        };
        if let Err(error) = response {
            ctx.log_error(&format!("回报属性服务结果失败：{error}"));
        }
        self.render(ctx).await;
    }

    async fn on_frame_tick(&self, ctx: &Context, _params: FrameTickParams) {
        self.ui.lock().await.tick();
        let (out, cursors_changed) = self.collab.lock().unwrap().tick();
        self.send_collab(ctx, out).await;
        if cursors_changed {
            self.sync_cursors();
        }
        self.render(ctx).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        self.handle_input(ctx, params).await;
    }

    async fn on_collab_message(&self, ctx: &Context, params: CollabMessageParams) {
        if params.channel == collab::presence::CHANNEL {
            let changed = self.collab.lock().unwrap().apply_presence(
                &params.peer_id,
                params.peer_name.as_deref(),
                params.peer_color.as_deref(),
                &params.payload,
            );
            if changed {
                self.sync_cursors();
                self.render(ctx).await;
            }
            return;
        }
        if params.channel != collab::SYNC_CHANNEL {
            return;
        }
        let Some((file, frames)) = collab::decode_frames(&params.payload) else {
            ctx.log_error("协作帧载荷不可解析");
            return;
        };
        let mut out = Vec::new();
        let mut text = None;
        for frame in frames {
            let (outcome, presence) = self
                .collab
                .lock()
                .unwrap()
                .apply(&params.peer_id, &file, &frame);
            if !outcome.reply.is_empty() {
                out.push(collab::frames(&file, outcome.reply));
            }
            out.extend(presence);
            if outcome.merge_failed {
                ctx.log_error(&format!("协作基线正文无法与本端对齐，已保留本端正文：{file}"));
            }
            if outcome.doc_changed {
                text = outcome.text;
            }
        }
        self.send_collab(ctx, out).await;
        let current = self.model.lock().unwrap().path.clone();
        if current.as_deref() != Some(file.as_str()) {
            // 对端在别的文件上编辑：帧只进协作文档，不改本端正文
            return;
        }
        if let Some(text) = text {
            {
                let mut model = self.model.lock().unwrap();
                model.replace_text(text);
                // 整篇换文会让槽位下标失效：丢弃在途槽编辑，避免提交到别的属性上
                model.props.editing = None;
                model.props.draft.clear();
                props::reset_suggest(&mut *model);
                model.dirty = true;
                // 远端合入即一次新的落盘尝试：上次失败原因不再拦住宿主排防抖保存
                model.save_error = None;
            }
            self.sync_cursors();
            self.reset_inputs().await;
            // 远端连续输入合并成一轮落盘：脏状态交给宿主 500ms 尾沿统一触发，
            // 逐帧整篇原子写会在多人输入时打成风暴
            let key = self.model.lock().unwrap().doc.key().cloned();
            if let Some(key) = key {
                self.report_state(ctx, &key).await;
            }
            self.render(ctx).await;
        }
    }
}

impl NotesPlugin {
    /// 协作上行统一出口：正文帧走可靠档，在场走瞬时档。发送失败只记日志，
    /// 本端正文与落盘不因协作通道受阻。
    async fn send_collab(&self, ctx: &Context, out: Vec<collab::Outgoing>) {
        for item in out {
            let sent = match item {
                collab::Outgoing::Frames(payload) => {
                    ctx.collab_send(collab::SYNC_CHANNEL, payload, None).await
                }
                collab::Outgoing::Presence(payload) => {
                    ctx.collab_send_ephemeral(collab::presence::CHANNEL, payload, None).await
                }
            };
            if let Err(error) = sent {
                ctx.log_error(&format!("发送协作帧失败：{error}"));
            }
        }
    }

    /// 远端光标写进模型：绘制只读模型，不每帧碰协作锁。
    fn sync_cursors(&self) {
        let mut model = self.model.lock().unwrap();
        let cursors = match model.path {
            Some(_) => self.collab.lock().unwrap().cursors(&model.text),
            None => Vec::new(),
        };
        model.remote = cursors;
    }

    /// 房间类型与本端身份重认：切仓库、进入 / 离开空间、名册变化后重取；
    /// 名册里消失的连接立即摘掉光标条目，不等超时。
    async fn refresh_room(&self, ctx: &Context) {
        let room = match ctx.space_room().await {
            Ok(room) => room,
            Err(error) => {
                ctx.log_error(&format!("读取协作房间失败：{error}"));
                return;
            }
        };
        let known = match ctx.collab_peers().await {
            Ok(peers) => peers.peers.into_iter().map(|peer| peer.peer_id).collect::<Vec<_>>(),
            Err(error) => {
                ctx.log_error(&format!("读取协作名册失败：{error}"));
                Vec::new()
            }
        };
        let mut collab = self.collab.lock().unwrap();
        collab.set_room(room.kind, room.self_identity);
        let dropped = collab.retain_roster(&known);
        drop(collab);
        if dropped {
            self.sync_cursors();
        }
    }

    /// 宿主改名广播：当前文档跟到新路径（会话身份、冷缓冲与协作挂靠一并跟随，正文不动）。
    async fn follow_rename(&self, ctx: &Context, old: &str, new: &str) {
        let (from, to) = {
            let mut model = self.model.lock().unwrap();
            let Some(from) = model.path.clone() else { return };
            let Some(to) = crate::session::follow_rename(&from, old, new) else { return };
            model.path = Some(to.clone());
            model.doc.rename_doc_id(&to);
            (from, to)
        };
        self.buffers.lock().unwrap().rename_path(&from, &to);
        let out = self.collab.lock().unwrap().follow_rename(&from, &to);
        self.send_collab(ctx, out).await;
        self.sync_cursors();
    }
}

impl NotesPlugin {
    /// 宿主改写正文后把输入面光标夹回文本长度（控件层每帧仍读插件给出的值）。
    async fn reset_inputs(&self) {
        let model = self.model.lock().unwrap().clone_for_view();
        let mut frame_ui = self.ui.lock().await;
        let focused = frame_ui.focus_id().map(str::to_string);
        if let Some(id) = focused.or_else(|| active_input(&model)) {
            if let Some(value) = value_of(&model, &id) {
                let caret = value.len();
                frame_ui.set_input(&id, &value, caret);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "笔记".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(NotesPlugin::new()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::frame::Modifiers;

    fn key(text: &str, modifiers: Modifiers) -> FrameInput {
        FrameInput::Key {
            key: Key::Character { text: text.into() },
            action: KeyAction::Press,
            modifiers,
            text: Some(text.into()),
        }
    }

    #[test]
    fn undo_shortcuts_only_fire_with_command_modifier() {
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        assert_eq!(undo_key(&key("z", ctrl)), Some(false));
        assert_eq!(undo_key(&key("z", Modifiers { shift: true, ..ctrl })), Some(true));
        assert_eq!(undo_key(&key("y", ctrl)), Some(true));
        assert_eq!(undo_key(&key("z", Modifiers::default())), None, "裸 z 是输入");
        let release = FrameInput::Key {
            key: Key::Character { text: "z".into() },
            action: KeyAction::Release,
            modifiers: ctrl,
            text: None,
        };
        assert_eq!(undo_key(&release), None, "抬起不触发");
    }

    #[test]
    fn tab_indent_selection_keeps_the_drag_direction() {
        let (text, anchor, caret) = selection_of_edit(edit::indent_edit("- a\n- b\n", 0, 8), true);
        assert_eq!(text, "  - a\n  - b\n");
        assert_eq!((anchor, caret), (2, 12));
        // 反向拖选：缩进后仍保持 anchor 在右、caret 在左
        let (_, anchor, caret) = selection_of_edit(edit::indent_edit("- a\n- b\n", 0, 8), false);
        assert_eq!((anchor, caret), (12, 2));
    }

    #[test]
    fn block_selection_maps_to_absolute_offsets() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "---\nt: x\n---\n# 标题\n\n段落\n".into(), true);
        let base = model.block_source(1).expect("段落块").0;
        let (anchor, head) = selection_of(&model, &ui::block_id(1), 2, 5).expect("选区");
        assert_eq!((anchor, head), (base + 5, base + 2), "anchor / head 保持方向");
    }

    #[test]
    fn view_snapshot_keeps_the_title_draft() {
        let mut model = NoteModel::default();
        model.renaming = Some("草稿".into());
        assert_eq!(model.clone_for_view().renaming.as_deref(), Some("草稿"));
    }

    #[test]
    fn manifest_matches_the_crate_identity() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["apiVersion"], atelyx_plugin_sdk::abi::CONTRACT_VERSION);
        assert_eq!(manifest["docTypeRegistrations"][0]["extensions"][0], "md");
        assert_eq!(manifest["docTypeRegistrations"][0]["ownerPlugin"], PLUGIN_ID);
        assert_eq!(manifest["docTypeRegistrations"][0]["views"][0], PANEL);
    }
}
