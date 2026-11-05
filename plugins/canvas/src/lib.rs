//! 画布插件入口：整块白板（视口 / 卡 / 分组 / 连线 / 小地图 / 对话卡）与保存链都在本插件，
//! 宿主只负责面板壳、输入转发与按帧绘制；画布模型与几何来自格式库 `atelyx-format::canvas`。

mod actions;
mod chat;
mod input;
mod model;
mod paint;
mod presence;
mod refs;
mod service;
mod session;

use std::sync::{Arc, Mutex};

use atelyx_plugin_protocol::event::{self, PublishParams};
use atelyx_plugin_protocol::frame::{FrameInput, FrameInputParams, FrameTickParams, Modifiers};
use atelyx_plugin_protocol::method::{
    self, AiAgent, AiModelPick, AiModelProvider, AiSettingsResult, ChatEventParams,
    ContributionSlot, MenuClosedParams, PanelFlushDoneParams, PanelFlushParams, PanelHeaderParams,
    PanelHeaderState, SpaceSelfIdentity, SurfaceAttachedParams, SurfaceDetachedParams,
};
use atelyx_plugin_protocol::service::HostCallParams;
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};

use crate::model::{AUTOSAVE_DEBOUNCE_MS, SaveReason, Session};
use crate::presence::Presence;

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.canvas";
/// 画布视图贡献 id（面标识 `plugin:atelyx.canvas:canvas` 的贡献段）。
const VIEW: &str = "canvas";

/// 运行态：会话 + 挂载标记 + 保存防抖 + 控件层 + 协作在场 + 选择缓存。
pub(crate) struct PluginState {
    pub session: Mutex<Option<Session>>,
    attached: Mutex<bool>,
    save_debounce: Mutex<SaveDebounce>,
    /// 面最近一次上报的尺寸（会话未建立时也记住）。
    surface_size: Mutex<(f32, f32)>,
    /// 控件层（浮层正文与输入区的焦点 / 滚动 / 输入态）。
    ui: tokio::sync::Mutex<FrameUi>,
    /// 最近一次输入携带的修饰键快照（指针输入本身不带修饰键）。
    mods: Mutex<Modifiers>,
    /// 协作在场（本端上行时刻 + 保活载荷）。
    presence: Mutex<Presence>,
    /// 本端协作身份（名字 / 成员色），随在场载荷带出。
    identity: Mutex<Option<SpaceSelfIdentity>>,
    /// `#` 候选的仓库文件与目录（随文件清单重建刷新）。
    vault_files: Mutex<Vec<String>>,
    vault_dirs: Mutex<Vec<String>>,
    agents: Mutex<Vec<AiAgent>>,
    models: Mutex<Vec<AiModelProvider>>,
    default_pick: Mutex<Option<AiModelPick>>,
    auto_materialize: Mutex<bool>,
}

#[derive(Default)]
struct SaveDebounce {
    generation: u64,
    running: bool,
}

impl PluginState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            session: Mutex::new(None),
            attached: Mutex::new(false),
            save_debounce: Mutex::new(SaveDebounce::default()),
            surface_size: Mutex::new((1.0, 1.0)),
            ui: tokio::sync::Mutex::new(FrameUi::new()),
            mods: Mutex::new(Modifiers::default()),
            presence: Mutex::new(Presence::default()),
            identity: Mutex::new(None),
            vault_files: Mutex::new(Vec::new()),
            vault_dirs: Mutex::new(Vec::new()),
            agents: Mutex::new(Vec::new()),
            models: Mutex::new(Vec::new()),
            default_pick: Mutex::new(None),
            auto_materialize: Mutex::new(true),
        })
    }

    fn is_attached(&self) -> bool {
        self.attached
            .lock()
            .map(|attached| *attached)
            .unwrap_or(false)
    }

    pub(crate) fn with_session<R>(&self, read: impl FnOnce(&Session) -> R) -> Option<R> {
        let guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_ref().map(read)
    }

    pub(crate) fn with_session_mut<R>(&self, write: impl FnOnce(&mut Session) -> R) -> Option<R> {
        let mut guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_mut().map(write)
    }

    pub(crate) fn with_presence_mut<R>(&self, write: impl FnOnce(&mut Presence) -> R) -> Option<R> {
        let mut guard = match self.presence.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        Some(write(&mut guard))
    }

    /// 本端协作身份；无空间或查询失败 = None（在场载荷回落缺省名与按 peer 取色）。
    pub(crate) fn identity(&self) -> Option<SpaceSelfIdentity> {
        self.identity
            .lock()
            .ok()
            .and_then(|identity| identity.clone())
    }

    /// 刷新本端身份缓存（进入空间 / 名册变化 / 挂面时）。
    async fn refresh_identity(&self, ctx: &Context) {
        let identity = ctx.space_room().await.ok().and_then(|room| room.self_identity);
        if let Ok(mut cached) = self.identity.lock() {
            *cached = identity;
        }
    }

    /// 释放本端已不该持有的卡编辑锁：非流式、草稿空、输入框未聚焦即释放。
    fn sweep_locks(&self, focused: Option<&str>) -> bool {
        let focused = focused.and_then(|id| id.strip_prefix("canvas-input-"));
        self.with_session_mut(|session| {
            let keep: std::collections::HashSet<String> = session
                .view
                .card_chat
                .iter()
                .filter(|(id, chat)| {
                    chat.streaming || !chat.draft.is_empty() || focused == Some(id.as_str())
                })
                .map(|(id, _)| id.clone())
                .collect();
            let before = session.view.locks.len();
            session.view.locks.retain(|id, _| keep.contains(id));
            before != session.view.locks.len()
        })
        .unwrap_or(false)
    }

    /// `#` 候选可用的仓库文件与目录（路径，均仓库相对）。
    pub(crate) fn vault_choices(&self) -> (Vec<String>, Vec<String>) {
        let files = self
            .vault_files
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        let dirs = self
            .vault_dirs
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        (files, dirs)
    }

    /// 落卡开关：查询失败按开启处理。
    pub(crate) async fn auto_materialize(&self, ctx: &Context) -> bool {
        match ctx
            .call_raw(method::ai::SETTINGS, serde_json::Value::Null)
            .await
        {
            Ok(value) => match serde_json::from_value::<AiSettingsResult>(value) {
                Ok(settings) => {
                    if let Ok(mut cached) = self.auto_materialize.lock() {
                        *cached = settings.auto_materialize;
                    }
                    settings.auto_materialize
                }
                Err(_) => true,
            },
            Err(_) => self
                .auto_materialize
                .lock()
                .map(|value| *value)
                .unwrap_or(true),
        }
    }

    /// 刷新 `#` 候选清单与 Agent / 模型选择。
    async fn load_choices(&self, ctx: &Context) {
        if let Ok(files) = ctx.vault_files().await {
            if let Ok(mut list) = self.vault_files.lock() {
                *list = files.files.into_iter().map(|entry| entry.path).collect();
            }
        }
        if let Ok(dirs) = ctx.vault_list_dirs().await {
            if let Ok(mut list) = self.vault_dirs.lock() {
                *list = dirs;
            }
        }
        if let Ok(agents) = ctx.ai_agents().await {
            if let Ok(mut list) = self.agents.lock() {
                *list = agents.agents;
            }
        }
        if let Ok(models) = ctx.ai_models().await {
            if let Ok(mut list) = self.models.lock() {
                *list = models.providers;
            }
            if let Ok(mut current) = self.default_pick.lock() {
                *current = models.current;
            }
        }
    }

    /// 声明面板头部：只带脏标记（无可见内容时宿主不占高度）。
    async fn panel_header(&self, ctx: &Context) {
        let dirty = self.with_session(|session| session.dirty).unwrap_or(false);
        let header = if dirty {
            // 只带脏标记：画布工具条自己显示「未保存」，宿主头部保持不占高度。
            Some(PanelHeaderState {
                dirty: true,
                ..Default::default()
            })
        } else {
            None
        };
        let params = PanelHeaderParams {
            target: VIEW.to_string(),
            header,
        };
        if let Err(error) = ctx.panel_header(&params).await {
            ctx.log_error(&format!("声明画布头部失败：{error}"));
        }
    }

    /// 提交整面帧：世界层自绘 + 卡正文浮层。
    pub(crate) async fn render(&self, ctx: &Context) {
        if !self.is_attached() {
            return;
        }
        let size = self
            .surface_size
            .lock()
            .map(|size| *size)
            .unwrap_or((1.0, 1.0));
        // 适应视口在绘制前消化：尺寸变化只排队，不打断用户的缩放 / 平移
        // 面还没拿到真实尺寸（1×1 占位）时不算定过视口，留给首个 Resize
        self.with_session_mut(|session| {
            if session.view.fit_pending && size.0 > 1.0 && size.1 > 1.0 {
                session.apply_fit(size.0, size.1);
            }
        });
        let session = self.with_session(|session| session.clone());
        let mut ui = self.ui.lock().await;
        let Some(session) = session else {
            // 未打开画布也要提交空态帧：否则宿主会继续显示上一张帧
            if let Err(error) = ui.render(ctx, VIEW, crate::paint::empty).await {
                ctx.log_error(&format!("提交画布视图失败：{error}"));
            }
            return;
        };
        let (agents, default_pick) = (
            self.agents
                .lock()
                .map(|list| list.clone())
                .unwrap_or_default(),
            self.default_pick.lock().ok().and_then(|pick| pick.clone()),
        );
        if let Err(error) = ui
            .render(ctx, VIEW, |ui| {
                crate::paint::frame(
                    ui,
                    &session,
                    size,
                    crate::paint::Choices {
                        agents: &agents,
                        default_pick: default_pick.as_ref(),
                    },
                )
            })
            .await
        {
            ctx.log_error(&format!("提交画布视图失败：{error}"));
        }
    }

    /// 自动保存防抖：新改动推后世代，同一时刻最多一个等待线程。
    pub(crate) fn schedule_save(self: &Arc<Self>, ctx: &Context) {
        self.panel_header_now(ctx);
        let generation = {
            let Ok(mut debounce) = self.save_debounce.lock() else {
                return;
            };
            debounce.generation += 1;
            if debounce.running {
                return;
            }
            debounce.running = true;
            debounce.generation
        };
        let state = self.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                if let Ok(mut debounce) = state.save_debounce.lock() {
                    debounce.running = false;
                }
                return;
            };
            let mut saved = generation;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(AUTOSAVE_DEBOUNCE_MS));
                let pending = match state.save_debounce.lock() {
                    Ok(debounce) => debounce.generation,
                    Err(poisoned) => poisoned.into_inner().generation,
                };
                if pending != saved {
                    saved = pending;
                    continue;
                }
                runtime.block_on(async {
                    match crate::session::save(&state, &ctx, SaveReason::AutoSave).await {
                        Ok(_) => crate::session::notify_changed(&state, &ctx).await,
                        Err(error) => {
                            ctx.log_error(&format!("保存画布失败：{error}"));
                            state.with_session_mut(|session| session.save_error = Some(error.clone()));
                            let _ = ctx
                                .notify(
                                    "保存画布失败",
                                    Some(&error),
                                    method::NotificationLevel::Error,
                                )
                                .await;
                        }
                    }
                    state.panel_header(&ctx).await;
                    state.render(&ctx).await;
                });
                let mut debounce = match state.save_debounce.lock() {
                    Ok(debounce) => debounce,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if debounce.generation == saved {
                    debounce.running = false;
                    break;
                }
                saved = debounce.generation;
            }
        });
    }

    fn panel_header_now(&self, ctx: &Context) {
        let ctx = ctx.clone();
        let dirty = self.with_session(|session| session.dirty).unwrap_or(false);
        // 回调不保证跑在 Tokio 运行时里，另起线程自建运行时发这一次声明。
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let header = if dirty {
                Some(PanelHeaderState {
                    dirty: true,
                    ..Default::default()
                })
            } else {
                None
            };
            let params = PanelHeaderParams {
                target: VIEW.to_string(),
                header,
            };
            runtime.block_on(async {
                let _ = ctx.panel_header(&params).await;
            });
        });
    }
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

impl PluginState {
    pub(crate) async fn ready(self: &Arc<Self>, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, VIEW, "画布", Some("shapes"), None)
            .await
        {
            ctx.log_error(&format!("注册画布面板失败：{error}"));
        }
        if let Err(error) = ctx.register_service(service::SERVICE, &["snapshot"]).await {
            ctx.log_error(&format!("注册画布服务失败：{error}"));
        }
        for name in [
            event::name::EDITOR_OPEN_CHANGED,
            event::name::CANVAS_RESTORE,
            event::name::CANVAS_COMMAND,
            event::name::SPACE_FILE_PATCH,
            event::name::PANEL_FLUSH,
            event::name::VAULT_SWITCH,
            event::name::VAULT_FILES_CHANGED,
            event::name::SPACE_PEERS_CHANGED,
            event::name::WINDOW_FILE_DROP,
            method::ai::HOST_CHAT_EVENT,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        self.load_choices(ctx).await;
        self.refresh_identity(ctx).await;
    }

    pub(crate) async fn surface_attached(
        self: &Arc<Self>,
        ctx: &Context,
        params: SurfaceAttachedParams,
    ) {
        if params.surface_id != VIEW {
            return;
        }
        if let Ok(mut attached) = self.attached.lock() {
            *attached = true;
        }
        crate::session::reload_from_open_files(self, ctx).await;
        self.refresh_identity(ctx).await;
        crate::service::emit_changed(self, ctx).await;
        self.panel_header(ctx).await;
        self.render(ctx).await;
        // 进入画布立即上报一帧：空选区也要让对端看到本端已进入
        crate::presence::send(self, ctx).await;
    }

    pub(crate) async fn surface_detached(
        self: &Arc<Self>,
        ctx: &Context,
        params: SurfaceDetachedParams,
    ) {
        if params.surface_id == VIEW {
            if let Ok(mut attached) = self.attached.lock() {
                *attached = false;
            }
            if let Some(path) = self.with_session(|session| session.file.clone()) {
                crate::presence::leave(self, ctx, &path).await;
            }
            // 离开画布：本端编辑锁全部释放，避免对端一直显示「编辑中」
            self.with_session_mut(|session| session.view.locks.clear());
        }
    }

    pub(crate) async fn host_event(self: &Arc<Self>, ctx: &Context, params: PublishParams) {
        match params.name.as_str() {
            event::name::EDITOR_OPEN_CHANGED => {
                crate::session::reload_from_open_files(self, ctx).await;
                crate::service::emit_changed(self, ctx).await;
                self.panel_header(ctx).await;
                self.render(ctx).await;
            }
            event::name::CANVAS_RESTORE => {
                if crate::session::restore_snapshot(self, &params.payload) {
                    // 回滚结果立即落盘，不经防抖（历史回滚的语义就是「现在就写回」）
                    let mut result = crate::session::save(self, ctx, SaveReason::Flush).await;
                    for _ in 0..7 {
                        if !matches!(result, Ok(false)) {
                            break;
                        }
                        result = crate::session::save(self, ctx, SaveReason::Flush).await;
                    }
                    crate::service::emit_changed(self, ctx).await;
                    self.panel_header(ctx).await;
                }
                self.render(ctx).await;
            }
            event::name::CANVAS_COMMAND => {
                let Ok(params) =
                    serde_json::from_value::<method::CanvasCommandParams>(params.payload.clone())
                else {
                    return;
                };
                self.command(ctx, &params.command).await;
            }
            event::name::SPACE_FILE_PATCH => {
                if crate::session::apply_remote(self, ctx, &params.payload).await {
                    crate::service::emit_changed(self, ctx).await;
                    self.render(ctx).await;
                }
            }
            event::name::VAULT_SWITCH => {
                if let Some(path) = self.with_session(|session| session.file.clone()) {
                    crate::presence::leave(self, ctx, &path).await;
                }
                crate::chat::detach_all(self, ctx).await;
                crate::presence::clear(self);
                self.with_session_mut(|session| {
                    session.view.remote.clear();
                    session.view.locks.clear();
                });
                self.refresh_identity(ctx).await;
                self.load_choices(ctx).await;
            }
            event::name::SPACE_PEERS_CHANGED => {
                self.refresh_identity(ctx).await;
                // 名册变化立即摘除已不在房间的对端，TTL 只兜底漏帧
                if let Ok(peers) = ctx.collab_peers().await {
                    let known: Vec<String> = peers
                        .peers
                        .into_iter()
                        .map(|peer| peer.peer_id)
                        .collect();
                    if crate::presence::retain_roster(self, &known) {
                        self.render(ctx).await;
                    }
                }
            }
            event::name::VAULT_FILES_CHANGED => {
                self.load_choices(ctx).await;
            }
            event::name::PANEL_FLUSH => {
                let Ok(params) = serde_json::from_value::<PanelFlushParams>(params.payload.clone())
                else {
                    return;
                };
                let mut result = crate::session::save(self, ctx, SaveReason::Flush).await;
                for _ in 0..7 {
                    if !matches!(result, Ok(false)) {
                        break;
                    }
                    result = crate::session::save(self, ctx, SaveReason::Flush).await;
                }
                if matches!(result, Ok(false)) {
                    result = Err("保存未收敛（持续有未落盘编辑）".to_string());
                }
                if result.is_ok() {
                    crate::session::notify_changed(self, ctx).await;
                    self.panel_header(ctx).await;
                }
                let done = PanelFlushDoneParams {
                    target: Some(params.target.clone()),
                    token: params.token,
                    ok: result.is_ok(),
                    error: result.err(),
                };
                let _ = ctx.panel_flush_done(&done).await;
                self.render(ctx).await;
            }
            event::name::WINDOW_FILE_DROP => self.files_dropped(ctx, &params.payload).await,
            _ => {}
        }
    }

    /// 外部图片拖入：只在悬停于对话卡时收下（与视口其它区域无关）。
    async fn files_dropped(self: &Arc<Self>, ctx: &Context, payload: &serde_json::Value) {
        if !self.is_attached() {
            return;
        }
        let Ok(payload) = serde_json::from_value::<event::FileDropPayload>(payload.clone()) else {
            ctx.log_error("文件拖入载荷不可解析");
            return;
        };
        let Some(Some(node)) = self.with_session(|session| {
            let hover = session.view.hover.clone()?;
            session
                .node(&hover)
                .filter(|node| node.node_type == "conversation")
                .map(|node| node.id.clone())
        }) else {
            return;
        };
        let mut images = Vec::new();
        for path in payload.paths {
            let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
            let Some(ext) = crate::chat::image_ext_of(&name) else {
                continue;
            };
            match ctx.read_binary(&path).await {
                Ok(bytes) => images.push(crate::model::PendingImage {
                    mime: crate::chat::image_mime(&ext).to_string(),
                    file_name: name,
                    bytes,
                }),
                Err(error) => {
                    let _ = ctx
                        .notify(
                            "读取图片失败",
                            Some(&error.to_string()),
                            method::NotificationLevel::Warning,
                        )
                        .await;
                }
            }
        }
        if images.is_empty() {
            return;
        }
        self.with_session_mut(|session| {
            if let Some(chat) = session.view.card_chat.get_mut(&node) {
                chat.pending_images.extend(images);
            }
        });
        self.render(ctx).await;
    }

    /// 宿主拦截的画布快捷键命令。
    async fn command(self: &Arc<Self>, ctx: &Context, command: &str) {
        // 卡内输入框持有焦点时快捷键归文本编辑：粘贴 / 撤销不能作用到画布选中集
        if self.ui.lock().await.focus_id().is_some() {
            return;
        }
        // 拖拽 / 改尺寸途中 Delete 不生效：既避免误删在拖的卡，也避免打断拖拽
        if command == "delete"
            && self
                .with_session(|session| session.view.drag.is_some())
                .unwrap_or(false)
        {
            return;
        }
        match command {
            "undo" => self.undo_redo(ctx, true).await,
            "redo" => self.undo_redo(ctx, false).await,
            "copy" => self.copy(ctx).await,
            "paste" => self.paste(ctx).await,
            "select-all" => {
                self.with_session_mut(|session| {
                    let visible: Vec<String> = session
                        .data
                        .nodes
                        .iter()
                        .filter(|node| {
                            node.node_type != "group"
                                && !crate::model::hidden_of(&session.data.nodes, &node.id)
                        })
                        .map(|node| node.id.clone())
                        .collect();
                    session.view.selection = visible;
                });
                crate::service::emit_changed(self, ctx).await;
                self.render(ctx).await;
            }
            "delete" => {
                self.delete_selection(ctx).await;
            }
            "escape" => {
                self.with_session_mut(|session| {
                    session.view.drag = None;
                    session.view.link_to = None;
                    session.view.marquee_to = None;
                    for chat in session.view.card_chat.values_mut() {
                        chat.rollback_confirm = None;
                        chat.mention_picker = None;
                    }
                });
                self.render(ctx).await;
            }
            _ => {}
        }
    }

    async fn undo_redo(self: &Arc<Self>, ctx: &Context, undo: bool) {
        let changed = self
            .with_session_mut(|session| {
                if session.view.drag.is_some() || crate::chat::any_streaming(session) {
                    return false;
                }
                let done = if undo { session.undo() } else { session.redo() };
                if done {
                    session.touch();
                    // 快照可能带回已删的对话卡 / 去掉新增的卡：运行态跟着数据对齐
                    crate::chat::sync_card_chats(session);
                }
                done
            })
            .unwrap_or(false);
        if changed {
            crate::service::emit_changed(self, ctx).await;
            self.schedule_save(ctx);
            self.render(ctx).await;
        }
    }

    async fn copy(self: &Arc<Self>, ctx: &Context) {
        let Some(payload) = self.with_session(|session| crate::actions::copy_selection(session))
        else {
            return;
        };
        match ctx.clipboard_write(payload.as_deref().unwrap_or("")).await {
            Ok(()) => {}
            Err(error) => {
                let _ = ctx
                    .notify(
                        "复制失败",
                        Some(&error.to_string()),
                        method::NotificationLevel::Warning,
                    )
                    .await;
            }
        }
    }

    async fn paste(self: &Arc<Self>, ctx: &Context) {
        let Ok(text) = ctx.clipboard_read().await else {
            self.with_session_mut(|session| {
                for chat in session.view.card_chat.values_mut() {
                    chat.error = None;
                }
            });
            return;
        };
        let changed = self
            .with_session_mut(|session| {
                let changed = crate::actions::paste(session, &text);
                if changed {
                    crate::chat::sync_card_chats(session);
                }
                changed
            })
            .unwrap_or(false);
        if changed {
            crate::service::emit_changed(self, ctx).await;
            self.schedule_save(ctx);
            self.render(ctx).await;
        }
    }

    async fn delete_selection(self: &Arc<Self>, ctx: &Context) {
        let removed = self
            .with_session_mut(|session| crate::actions::delete_selection(session))
            .unwrap_or_default();
        for node in &removed {
            crate::chat::detach_node(self, ctx, node);
        }
        if !removed.is_empty() {
            crate::service::emit_changed(self, ctx).await;
            self.schedule_save(ctx);
            self.render(ctx).await;
        }
    }

    pub(crate) async fn frame_input(self: &Arc<Self>, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != VIEW {
            return;
        }
        let input = params.input.clone();
        if let FrameInput::Resize { width, height } = input {
            if let Ok(mut size) = self.surface_size.lock() {
                *size = (width, height);
            }
            self.with_session_mut(|session| {
                session.view.viewport = (width, height);
                // 只在还没有真实视口时适应：改尺寸不该重置用户的缩放与平移
                if !session.view.fitted {
                    session.view.fit_pending = true;
                }
            });
            // 排版尺寸由帧状态机持有：不转给它，后续每帧继续按占位尺寸排。
            let mut ui = self.ui.lock().await;
            ui.event(ctx, &input, |_| None).await;
            drop(ui);
            self.render(ctx).await;
            return;
        }
        let size = self
            .surface_size
            .lock()
            .map(|size| *size)
            .unwrap_or((1.0, 1.0));
        let bodies = self.with_session(input::card_bodies).unwrap_or_default();
        let inside_overlay = pointer(&input)
            .is_some_and(|(x, y)| bodies.iter().any(|(_, rect)| rect.contains(x, y)));
        let events = {
            let mut ui = self.ui.lock().await;
            let draft_of = |id: &str| {
                let node = id.strip_prefix("canvas-input-")?;
                self.with_session(|session| {
                    session
                        .view
                        .card_chat
                        .get(node)
                        .map(|chat| chat.draft.clone())
                })
                .flatten()
            };
            ui.event(ctx, &input, draft_of).await
        };
        let mut redraw = false;
        let mut dirty = false;
        for event in events {
            match event {
                UiEvent::Text { id, value } => {
                    if id == "canvas-rename-input" {
                        redraw |= self
                            .with_session_mut(|session| {
                                if let Some((_, draft)) = session.view.group_rename.as_mut() {
                                    *draft = value;
                                    true
                                } else {
                                    false
                                }
                            })
                            .unwrap_or(false);
                    } else if let Some(node) = id.strip_prefix("canvas-input-") {
                        redraw |= self
                            .with_session_mut(|session| {
                                let Some(chat) = session.view.card_chat.get_mut(node) else {
                                    return false;
                                };
                                chat.draft = value;
                                chat.caret = chat.caret.min(chat.draft.len());
                                crate::chat::refresh_picker(self, session, node);
                                if !crate::chat::locked_by_peer(session, node) {
                                    session.acquire_lock(node, crate::chat::now_ms());
                                }
                                true
                            })
                            .unwrap_or(false);
                    }
                }
                UiEvent::Caret { id, caret, .. } => {
                    if let Some(node) = id.strip_prefix("canvas-input-") {
                        redraw |= self
                            .with_session_mut(|session| {
                                let Some(chat) = session.view.card_chat.get_mut(node) else {
                                    return false;
                                };
                                chat.caret = caret;
                                crate::chat::refresh_picker(self, session, node);
                                true
                            })
                            .unwrap_or(false);
                    }
                }
                UiEvent::Submit { id } => {
                    if id == "canvas-rename-input" {
                        dirty |= self
                            .with_session_mut(|session| {
                                let Some((node, name)) = session.view.group_rename.clone() else {
                                    return false;
                                };
                                crate::actions::rename_commit(session, &node, &name);
                                session.touch();
                                true
                            })
                            .unwrap_or(false);
                        redraw |= dirty;
                    } else if let Some(node) = id.strip_prefix("canvas-input-") {
                        redraw |= crate::chat::send(self, ctx, node).await;
                        // 用户消息已落卡但没有后续 chat 事件时也要落盘
                        dirty |= self
                            .with_session(|session| session.dirty)
                            .unwrap_or(false);
                    } else if let Some(node) = id.strip_prefix("canvas-send-") {
                        redraw |= crate::chat::send(self, ctx, node).await;
                        dirty |= self
                            .with_session(|session| session.dirty)
                            .unwrap_or(false);
                    }
                }
                UiEvent::Scroll {
                    id,
                    offset,
                    max_offset,
                } => {
                    // 远离底部即退出跟随：「回到底部」按钮据此出现，新消息不再强拉视图
                    if let Some(node) = id.strip_prefix("canvas-chat-") {
                        redraw |= self
                            .with_session_mut(|session| {
                                let Some(chat) = session.view.card_chat.get_mut(node) else {
                                    return false;
                                };
                                let follow = offset + 8.0 >= max_offset;
                                let changed = chat.follow_bottom != follow;
                                chat.follow_bottom = follow;
                                changed
                            })
                            .unwrap_or(false);
                    }
                }
                UiEvent::Click { id } => {
                    let (redraw_here, dirty_here) = self.ui_click(ctx, &id).await;
                    redraw |= redraw_here;
                    dirty |= dirty_here;
                }
                _ => {}
            }
        }
        if !inside_overlay
            || matches!(
                input,
                FrameInput::PointerUp { .. } | FrameInput::PointerMove { .. }
            )
        {
            let mut mods = self.mods.lock().map(|mods| *mods).unwrap_or_default();
            let outcome = self
                .with_session_mut(|session| input::handle(session, &input, size, &mut mods))
                .unwrap_or_default();
            if let Ok(mut current) = self.mods.lock() {
                *current = mods;
            }
            if let Some((source, target)) = outcome.link {
                self.with_session_mut(|session| {
                    actions::append_card_ref(session, &source, &target)
                });
                redraw = true;
            }
            if let Some(path) = outcome.open_file {
                let params = atelyx_plugin_protocol::method::EditorOpenFileParams {
                    path,
                    reveal: false,
                };
                if let Ok(value) = serde_json::to_value(params) {
                    if let Err(error) = ctx
                        .call_raw(atelyx_plugin_protocol::method::editor::OPEN_FILE, value)
                        .await
                    {
                        ctx.log_error(&format!("打开文件失败：{error}"));
                    }
                }
            }
            if let Some(menu) = outcome.menu {
                let _ = ctx.open_menu(&menu).await;
            }
            redraw |= outcome.redraw;
            dirty = dirty || outcome.dirty;
        }
        // 输入处理完清扫一次编辑锁：失焦且草稿空的卡不再占锁
        let focused = {
            let ui = self.ui.lock().await;
            ui.focus_id().map(str::to_string)
        };
        let released = self.sweep_locks(focused.as_deref());
        let moving = matches!(input, FrameInput::PointerMove { .. });
        // 拖动 / 改尺寸在途：几何已变，属性面板要跟手；只发变更事件，不排保存
        let geometry_move = moving
            && self
                .with_session(|session| {
                    session.view.drag.as_ref().is_some_and(|drag| {
                        drag.mutated
                            && matches!(
                                drag.kind,
                                crate::model::DragKind::Card
                                    | crate::model::DragKind::Resize(_)
                            )
                    })
                })
                .unwrap_or(false);
        if ((redraw || dirty) && !moving) || geometry_move {
            crate::service::emit_changed(self, ctx).await;
        }
        if (redraw || dirty || released) && !moving && self.is_attached() {
            crate::presence::send(self, ctx).await;
        }
        if redraw || released {
            self.render(ctx).await;
        }
        if dirty {
            self.schedule_save(ctx);
        }
    }

    /// 浮层内点击：消息操作 / 卡头控制 / 输入工具行 / `#` 候选。
    async fn ui_click(self: &Arc<Self>, ctx: &Context, id: &str) -> (bool, bool) {
        if let Some((action, node, message)) = crate::chat::parse_message_action(id) {
            return self.message_action(ctx, action, node, message).await;
        }
        if let Some(rest) = id.strip_prefix("step|") {
            let Some((node, key)) = rest.split_once('|') else {
                return (false, false);
            };
            let changed = self
                .with_session_mut(|session| {
                    session.view.card_chat.get_mut(node).is_some_and(|chat| {
                        if !chat.expanded.remove(key) {
                            chat.expanded.insert(key.to_string());
                        }
                        true
                    })
                })
                .unwrap_or(false);
            return (changed, false);
        }
        if let Some(node) = id.strip_prefix("canvas-error-clear-") {
            let cleared = self
                .with_session_mut(|session| {
                    session
                        .view
                        .card_chat
                        .get_mut(node)
                        .is_some_and(|chat| chat.error.take().is_some())
                })
                .unwrap_or(false);
            return (cleared, false);
        }
        if let Some(rest) = id.strip_prefix("canvas-stop-") {
            crate::chat::stop(ctx, rest).await;
            return (false, false);
        }
        if let Some(rest) = id.strip_prefix("canvas-compact-") {
            crate::chat::compact(self, ctx, rest).await;
            return (true, true);
        }
        if let Some(rest) = id.strip_prefix("canvas-pick-") {
            let Some((node, index)) = rest.rsplit_once('-') else {
                return (false, false);
            };
            let Ok(index) = index.parse::<usize>() else {
                return (false, false);
            };
            let changed = self
                .with_session_mut(|session| crate::chat::commit_mention(session, node, index))
                .unwrap_or(false);
            return (changed, false);
        }
        if let Some(rest) = id.strip_prefix("canvas-provider-") {
            return (self.choose_provider(ctx, rest), false);
        }
        if let Some(rest) = id.strip_prefix("canvas-agent-") {
            return (self.choose_agent(ctx, rest), false);
        }
        if let Some(rest) = id.strip_prefix("canvas-bottom-") {
            self.with_session_mut(|session| {
                if let Some(chat) = session.view.card_chat.get_mut(rest) {
                    chat.follow_bottom = true;
                }
            });
            let mut ui = self.ui.lock().await;
            ui.scroll_to_end(&format!("canvas-chat-{rest}"));
            drop(ui);
            self.render(ctx).await;
            return (true, false);
        }
        if let Some(rest) = id.strip_prefix("canvas-attach-") {
            return (self.attach_files(ctx, rest).await, false);
        }
        if let Some(rest) = id.strip_prefix("canvas-remove-image-") {
            let Some((node, index)) = rest.rsplit_once('-') else {
                return (false, false);
            };
            let Ok(index) = index.parse::<usize>() else {
                return (false, false);
            };
            let changed = self
                .with_session_mut(|session| {
                    session.view.card_chat.get_mut(node).is_some_and(|chat| {
                        if index < chat.pending_images.len() {
                            chat.pending_images.remove(index);
                            true
                        } else {
                            false
                        }
                    })
                })
                .unwrap_or(false);
            return (changed, false);
        }
        (false, false)
    }

    /// 消息操作：复制 / 回滚（两段确认）/ 取消回滚 / 分支 / 重生成。
    async fn message_action(
        self: &Arc<Self>,
        ctx: &Context,
        action: crate::chat::MessageAction,
        node: &str,
        message: Option<&str>,
    ) -> (bool, bool) {
        use crate::chat::MessageAction;
        match action {
            MessageAction::Copy => {
                let Some(message) = message else {
                    return (false, false);
                };
                let text = self
                    .with_session(|session| {
                        crate::chat::messages_of(session, node)
                            .and_then(|messages| {
                                messages.iter().find(|m| {
                                    m.get("id").and_then(serde_json::Value::as_str)
                                        == Some(message)
                                })
                            })
                            .map(|message| {
                                message
                                    .get("displayContent")
                                    .and_then(serde_json::Value::as_str)
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| {
                                        message
                                            .get("content")
                                            .and_then(serde_json::Value::as_str)
                                    })
                                    .unwrap_or("")
                                    .to_string()
                            })
                    })
                    .flatten();
                if let Some(text) = text {
                    let _ = ctx.write_clipboard(&text).await;
                }
                (false, false)
            }
            MessageAction::Rollback => {
                let Some(message) = message else {
                    return (false, false);
                };
                // 首点只进入待确认态，重绘出确认行；同一消息再点才截断
                let changed = self
                    .with_session_mut(|session| {
                        crate::chat::rollback_click(session, node, message)
                    })
                    .unwrap_or(false);
                (true, changed)
            }
            MessageAction::RollbackConfirm => {
                let Some(message) = message else {
                    return (false, false);
                };
                let changed = self
                    .with_session_mut(|session| {
                        crate::chat::confirm_rollback(session, node, message)
                    })
                    .unwrap_or(false);
                (changed, changed)
            }
            MessageAction::CancelRollback => {
                self.with_session_mut(|session| crate::chat::cancel_rollback(session, node));
                (true, false)
            }
            MessageAction::Branch => {
                let Some(message) = message else {
                    return (false, false);
                };
                let branched = self
                    .with_session_mut(|session| crate::chat::branch(session, node, message))
                    .is_some();
                (branched, branched)
            }
            MessageAction::Regen => {
                let changed = crate::chat::regenerate(self, ctx, node).await;
                (changed, changed)
            }
        }
    }

    /// 供应商 / 模型选择菜单。
    fn choose_provider(self: &Arc<Self>, ctx: &Context, node: &str) -> bool {
        let providers = self
            .models
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        if providers.is_empty() {
            return false;
        }
        let current = self
            .with_session(|session| {
                let provider = session
                    .node(node)
                    .and_then(|n| n.data.get("providerId"))
                    .and_then(serde_json::Value::as_str);
                let model = session
                    .node(node)
                    .and_then(|n| n.data.get("model"))
                    .and_then(serde_json::Value::as_str);
                let effort = session
                    .node(node)
                    .and_then(|n| n.data.get("reasoningEffort"))
                    .and_then(serde_json::Value::as_str);
                (
                    provider.map(str::to_string),
                    model.map(str::to_string),
                    effort.map(str::to_string),
                )
            })
            .unwrap_or((None, None, None));
        let mut items = Vec::new();
        for provider in &providers {
            for model in &provider.models {
                let selected = current.0.as_deref() == Some(provider.id.as_str())
                    && current.1.as_deref() == Some(model.id.as_str());
                let id = format!("model:{}:{}:{}", node, provider.id, model.id);
                let label = format!(
                    "{} · {}",
                    provider.name,
                    model.nickname.clone().unwrap_or_else(|| model.id.clone())
                );
                items.push(if selected {
                    method::MenuItem::Checked { id, label, pane: 0 }
                } else {
                    method::MenuItem::Action {
                        id,
                        label,
                        icon: None,
                        danger: false,
                        disabled: false,
                        note: None,
                        hint: None,
                        pane: 0,
                    }
                });
            }
        }
        if items.is_empty() {
            return false;
        }
        // 二级菜单：推理等级（值写回节点数据，随消息一起发给模型）
        let current_effort = current.2.as_deref().unwrap_or("");
        items.push(method::MenuItem::Divider { pane: 0 });
        items.push(method::MenuItem::Submenu {
            label: "推理等级".to_string(),
            icon: Some("sliders-horizontal".to_string()),
            disabled: false,
            pane: 0,
            target: 1,
        });
        for (value, label) in [
            ("", "默认"),
            ("off", "关闭"),
            ("low", "低"),
            ("medium", "中"),
            ("high", "高"),
        ] {
            let id = format!("effort:{node}:{value}");
            let label = label.to_string();
            items.push(if current_effort == value {
                method::MenuItem::Checked { id, label, pane: 1 }
            } else {
                method::MenuItem::Action {
                    id,
                    label,
                    icon: None,
                    danger: false,
                    disabled: false,
                    note: None,
                    hint: None,
                    pane: 1,
                }
            });
        }
        let params = method::MenuOpenParams {
            items,
            width: Some(280.0),
            search: None,
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(error) = ctx.open_menu(&params).await {
                ctx.log_error(&format!("打开模型菜单失败：{error}"));
            }
        });
        false
    }

    /// Agent 选择菜单。
    fn choose_agent(self: &Arc<Self>, ctx: &Context, node: &str) -> bool {
        let agents = self
            .agents
            .lock()
            .map(|list| list.clone())
            .unwrap_or_default();
        let current = self
            .with_session(|session| {
                session
                    .node(node)
                    .and_then(|n| n.data.get("agentId"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .flatten();
        let item = |id: String, label: String, selected: bool| {
            if selected {
                method::MenuItem::Checked { id, label, pane: 0 }
            } else {
                method::MenuItem::Action {
                    id,
                    label,
                    icon: None,
                    danger: false,
                    disabled: false,
                    note: None,
                    hint: None,
                    pane: 0,
                }
            }
        };
        let mut items = vec![item(
            format!("agent:{}:", node),
            "默认".to_string(),
            current.is_none(),
        )];
        for agent in &agents {
            items.push(item(
                format!("agent:{}:{}", node, agent.id),
                agent.name.clone(),
                current.as_deref() == Some(agent.id.as_str()),
            ));
        }
        let params = method::MenuOpenParams {
            items,
            width: Some(240.0),
            search: None,
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(error) = ctx.open_menu(&params).await {
                ctx.log_error(&format!("打开 Agent 菜单失败：{error}"));
            }
        });
        false
    }

    /// 选图片附件：系统文件对话框多选。
    async fn attach_files(self: &Arc<Self>, ctx: &Context, node: &str) -> bool {
        let params = method::PickFileParams {
            title: Some("选择图片".to_string()),
            save: None,
            multiple: true,
            default_path: None,
            filters: vec![method::FileFilter {
                name: "图片".to_string(),
                extensions: ["png", "jpg", "jpeg", "webp", "gif"]
                    .iter()
                    .map(|e| e.to_string())
                    .collect(),
            }],
        };
        let paths = match ctx.pick_files(params).await {
            Ok(paths) if !paths.is_empty() => paths,
            _ => return false,
        };
        let mut images = Vec::new();
        for path in paths {
            let name = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
            let Some(ext) = crate::chat::image_ext_of(&name) else {
                continue;
            };
            match ctx.read_binary(&path).await {
                Ok(bytes) => images.push(crate::model::PendingImage {
                    mime: crate::chat::image_mime(&ext).to_string(),
                    file_name: name,
                    bytes,
                }),
                Err(error) => {
                    let _ = ctx
                        .notify(
                            "读取图片失败",
                            Some(&error.to_string()),
                            method::NotificationLevel::Warning,
                        )
                        .await;
                }
            }
        }
        self.with_session_mut(|session| {
            if let Some(chat) = session.view.card_chat.get_mut(node) {
                chat.pending_images.extend(images);
            }
        });
        true
    }

    pub(crate) async fn frame_tick(self: &Arc<Self>, ctx: &Context, params: FrameTickParams) {
        if params.surface_id != VIEW {
            return;
        }
        let focused = {
            let ui = self.ui.lock().await;
            ui.focus_id().map(str::to_string)
        };
        let released = self.sweep_locks(focused.as_deref());
        let pruned = crate::presence::prune(self);
        if pruned || released {
            self.render(ctx).await;
        }
        if self.is_attached() {
            // 每拍都试上行（最小间隔在 Presence 内限频）：锁释放等非输入变化不等保活
            crate::presence::send(self, ctx).await;
            crate::presence::keepalive(self, ctx).await;
        }
    }
}

impl PluginState {
    /// 右键菜单选中项；返回是否有数据变化（需要保存）。
    async fn menu_action(self: &Arc<Self>, ctx: &Context, id: &str) -> bool {
        if let Some(rest) = id.strip_prefix("model:") {
            let mut parts = rest.splitn(3, ':');
            let (Some(node), Some(provider), Some(model)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            return self
                .with_session_mut(|session| {
                    if let Some(node) = session.node_mut(node) {
                        node.data["providerId"] = serde_json::Value::String(provider.to_string());
                        node.data["model"] = serde_json::Value::String(model.to_string());
                        session.touch();
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
        }
        if let Some(rest) = id.strip_prefix("agent:") {
            let mut parts = rest.splitn(2, ':');
            let (Some(node), Some(agent)) = (parts.next(), parts.next()) else {
                return false;
            };
            return self
                .with_session_mut(|session| {
                    let Some(target) = session.node_mut(node) else {
                        return false;
                    };
                    if agent.is_empty() {
                        target
                            .data
                            .as_object_mut()
                            .map(|data| data.remove("agentId"));
                    } else {
                        target.data["agentId"] = serde_json::Value::String(agent.to_string());
                    }
                    session.touch();
                    true
                })
                .unwrap_or(false);
        }
        if let Some(rest) = id.strip_prefix("effort:") {
            let mut parts = rest.splitn(2, ':');
            let (Some(node), Some(value)) = (parts.next(), parts.next()) else {
                return false;
            };
            return self
                .with_session_mut(|session| {
                    let Some(target) = session.node_mut(node) else {
                        return false;
                    };
                    if value.is_empty() {
                        if let Some(data) = target.data.as_object_mut() {
                            data.remove("reasoningEffort");
                        }
                    } else {
                        target.data["reasoningEffort"] =
                            serde_json::Value::String(value.to_string());
                    }
                    session.touch();
                    true
                })
                .unwrap_or(false);
        }
        let Some(action) = id.strip_prefix("canvas-menu-") else {
            return false;
        };
        match action {
            "copy" => {
                self.copy(ctx).await;
                false
            }
            "paste" => {
                self.paste(ctx).await;
                false
            }
            "delete" => {
                self.delete_selection(ctx).await;
                false
            }
            "open" => {
                let path = self
                    .with_session(|session| {
                        session
                            .view
                            .selection
                            .first()
                            .and_then(|id| session.node(id))
                            .and_then(|node| node.data.get("file"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .flatten();
                if let Some(path) = path {
                    let params = method::EditorOpenFileParams {
                        path,
                        reveal: false,
                    };
                    if let Ok(value) = serde_json::to_value(params) {
                        let _ = ctx.call_raw(method::editor::OPEN_FILE, value).await;
                    }
                }
                false
            }
            "group-create" => self
                .with_session_mut(|session| crate::actions::create_group_from_selection(session))
                .is_some(),
            "group-rename" => self
                .with_session_mut(|session| {
                    let id = session.view.selection.first()?.clone();
                    let label = session
                        .node(&id)?
                        .data
                        .get("label")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    session.view.group_rename = Some((id, label));
                    Some(())
                })
                .is_some(),
            "group-collapse" | "group-expand" => self
                .with_session_mut(|session| {
                    let id = session.view.selection.first()?.clone();
                    crate::actions::toggle_collapse(session, &id);
                    session.touch();
                    Some(())
                })
                .is_some(),
            _ if action.starts_with("color-") => {
                let Ok(index) = action["color-".len()..].parse::<usize>() else {
                    return false;
                };
                self.with_session_mut(|session| {
                    let id = session.view.selection.first()?.clone();
                    crate::actions::set_group_color(session, &id, index);
                    Some(())
                })
                .is_some()
            }
            "new-conversation" | "new-text" | "new-group" => {
                let at = self
                    .with_session(|session| session.view.menu_at)
                    .flatten()
                    .unwrap_or((0.0, 0.0));
                let x = (at.0 / 16.0).round() * 16.0;
                let y = (at.1 / 16.0).round() * 16.0;
                let created = self
                    .with_session_mut(|session| match action {
                        "new-conversation" => {
                            Some(crate::actions::create_conversation(session, x, y))
                        }
                        "new-text" => Some(crate::actions::create_text(session, x, y)),
                        _ => Some(crate::actions::create_group_at(session, x, y)),
                    })
                    .flatten();
                if created.is_some() {
                    self.with_session_mut(crate::chat::sync_card_chats);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

pub(crate) struct CanvasPlugin {
    state: Arc<PluginState>,
}

#[async_trait::async_trait]
impl PluginHandler for CanvasPlugin {
    async fn on_ready(&self, ctx: &Context) {
        self.state.ready(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        self.state.surface_attached(ctx, params).await;
    }

    async fn on_surface_detached(&self, ctx: &Context, params: SurfaceDetachedParams) {
        crate::chat::detach_all(&self.state, ctx).await;
        self.state.surface_detached(ctx, params).await;
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        self.state.host_event(ctx, params).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        self.state.frame_input(ctx, params).await;
    }

    async fn on_frame_tick(&self, ctx: &Context, params: FrameTickParams) {
        self.state.frame_tick(ctx, params).await;
    }

    async fn on_chat_event(&self, ctx: &Context, params: ChatEventParams) {
        crate::chat::on_chat_event(&self.state, ctx, params).await;
        crate::service::emit_changed(&self.state, ctx).await;
        self.state.schedule_save(ctx);
        self.state.render(ctx).await;
        // 流式开始 / 结束要即时上屏给对端，不能等下一次保活；面已卸载时不再上行，
        // 否则离场后迟到的结束事件会把本端重新广播成在场
        if self.state.is_attached() {
            crate::presence::send(&self.state, ctx).await;
        }
    }

    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        crate::service::handle(&self.state, ctx, params).await;
    }

    async fn on_collab_message(
        &self,
        ctx: &Context,
        params: atelyx_plugin_protocol::method::CollabMessageParams,
    ) {
        if params.channel != crate::presence::CHANNEL {
            return;
        }
        crate::presence::apply(&self.state, params);
        self.state.render(ctx).await;
    }

    async fn on_menu_closed(&self, ctx: &Context, params: MenuClosedParams) {
        let Some(id) = params.selected else { return };
        if !self.state.menu_action(ctx, &id).await {
            return;
        }
        crate::service::emit_changed(&self.state, ctx).await;
        self.state.schedule_save(ctx);
        self.state.render(ctx).await;
    }
}

fn pointer(input: &FrameInput) -> Option<(f32, f32)> {
    match input {
        FrameInput::PointerDown { x, y, .. }
        | FrameInput::PointerUp { x, y, .. }
        | FrameInput::PointerMove { x, y, .. }
        | FrameInput::DoubleClick { x, y, .. }
        | FrameInput::Wheel { x, y, .. } => Some((*x, *y)),
        _ => None,
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "画布".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || {
            Box::new(CanvasPlugin {
                state: PluginState::new(),
            })
        },
    )
}
