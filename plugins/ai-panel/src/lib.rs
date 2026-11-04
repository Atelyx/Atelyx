//! AI 对话面板插件：会话侧栏、消息流、输入区与模型 / Agent 选择。
//! 会话真源在宿主；本插件只经 `chat.session.*` / `ai.chat.*` 读写，不持有磁盘数据。

mod model;
mod ui;

use std::sync::Mutex;

use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use atelyx_plugin_protocol::chat::{
    AppendBatch, CommitBatch, MetaPatch, OverridesPatch, SessionDef, TruncateEntry,
};
use atelyx_plugin_protocol::event::{
    self, ChatChangeKind, ChatChangedPayload, ChatPersistChangedPayload, FileChangeKind,
    FileChangedPayload, FileDropPayload, PublishParams,
};
use atelyx_plugin_protocol::frame::{
    FrameInput, FrameInputParams, FrameTickParams, Key, KeyAction, NamedKey,
};
use atelyx_plugin_protocol::method::{
    ChatEvent, ChatEventParams, ChatRef, ChatSendParams, ContributionSlot, FileFilter, FsEntryKind,
    PickFileParams, SurfaceAttachedParams, SurfaceDetachedParams,
};
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};

use crate::model::{
    display_of, Model, PendingImage, RefEntry, RefKind, DRAFT, PANEL,
};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.ai-panel";
/// 引用候选的文件树条目上限（避免大仓库一次装载拖垮面板）。
const FILE_LIMIT: usize = 2000;
/// 图片选择的扩展名（与消息附件收口一致）。
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];

struct AiPanelPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
    attached: Mutex<bool>,
}

impl AiPanelPlugin {
    fn new() -> Self {
        Self {
            model: Mutex::new(Model::new()),
            ui: AsyncMutex::new(FrameUi::new()),
            attached: Mutex::new(false),
        }
    }

    fn is_attached(&self) -> bool {
        *self.attached.lock().unwrap()
    }

    async fn render(&self, ctx: &Context) {
        if !self.is_attached() {
            return;
        }
        let snapshot = self.model.lock().unwrap().clone();
        let mut frame_ui = self.ui.lock().await;
        if let Err(error) = frame_ui.render(ctx, PANEL, |frame| ui::frame(frame, &snapshot)).await {
            ctx.log_error(&format!("提交对话面板失败：{error}"));
        }
    }

    fn set_error(&self, message: impl Into<String>) {
        self.model.lock().unwrap().error = Some(message.into());
    }

    async fn load_list(&self, ctx: &Context) {
        match ctx.chat_history(None, None, None, None).await {
            Ok(list) => {
                let mut model = self.model.lock().unwrap();
                model.apply_list(list);
                if let Some(active) = model.active.clone() {
                    if model.session(&active).is_none() {
                        model.remove_session(&active);
                    }
                }
                model.ready = true;
            }
            Err(error) => self.set_error(format!("会话清单读取失败：{error}")),
        }
    }

    async fn load_session(&self, ctx: &Context, session_id: &str) {
        match ctx.chat_session_read(session_id).await {
            Ok(record) => self.model.lock().unwrap().apply_session(record),
            Err(error) => self.set_error(format!("会话读取失败：{error}")),
        }
    }

    async fn load_choices(&self, ctx: &Context) {
        match ctx.ai_models().await {
            Ok(models) => self.model.lock().unwrap().apply_models(models),
            Err(error) => self.set_error(format!("模型清单读取失败：{error}")),
        }
        match ctx.ai_agents().await {
            Ok(agents) => self.model.lock().unwrap().apply_agents(agents),
            Err(error) => self.set_error(format!("Agent 清单读取失败：{error}")),
        }
    }

    async fn load_all(&self, ctx: &Context) {
        self.load_list(ctx).await;
        match ctx.chat_session_snapshot().await {
            Ok(snapshot) => {
                let mut model = self.model.lock().unwrap();
                model.apply_overrides(
                    snapshot.model_override,
                    snapshot.effort_override,
                    snapshot.agent_override,
                );
                model.set_persist_error(snapshot.persist_error.map(|error| error.message));
            }
            Err(error) => self.set_error(format!("会话快照读取失败：{error}")),
        }
        let active = self.model.lock().unwrap().active.clone();
        if let Some(active) = active {
            self.load_session(ctx, &active).await;
        }
    }

    async fn load_files(&self, ctx: &Context) {
        let mut files = Vec::new();
        let mut dirs = vec![String::new()];
        let mut seen = 0usize;
        let mut complete = true;
        while let Some(dir) = dirs.pop() {
            let entries = match ctx.vault_list(&dir).await {
                Ok(entries) => entries,
                Err(error) => {
                    ctx.log_error(&format!("读取目录 {dir} 失败：{error}"));
                    complete = false;
                    break;
                }
            };
            for entry in entries {
                if entry.name.starts_with('.') {
                    continue;
                }
                let path = if dir.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{dir}/{}", entry.name)
                };
                match entry.kind {
                    FsEntryKind::Dir => {
                        files.push(model::FileEntry { path: path.clone(), is_dir: true });
                        dirs.push(path);
                    }
                    FsEntryKind::File if model::kind_of_file(&path).is_some() => {
                        files.push(model::FileEntry { path, is_dir: false });
                    }
                    FsEntryKind::File => {}
                }
                seen += 1;
                if seen >= FILE_LIMIT {
                    break;
                }
            }
            if seen >= FILE_LIMIT {
                break;
            }
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut model = self.model.lock().unwrap();
        model.files = files;
        model.files_ready = complete;
    }

    async fn attach_image(&self, ctx: &Context) {
        let params = PickFileParams {
            multiple: false,
            title: Some("选择图片".to_string()),
            save: None,
            default_path: None,
            filters: vec![FileFilter {
                name: "图片".to_string(),
                extensions: IMAGE_EXTS.iter().map(|ext| (*ext).to_string()).collect(),
            }],
        };
        match ctx.pick_file(params).await {
            Ok(Some(path)) => {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("image")
                    .to_string();
                self.model.lock().unwrap().push_image(name, path);
            }
            Ok(None) => {}
            Err(error) => self.set_error(format!("选择图片失败：{error}")),
        }
    }

    async fn stage_images(
        &self,
        ctx: &Context,
        session_id: &str,
        images: &mut [PendingImage],
    ) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for image in images.iter_mut() {
            if let Some(staged) = image.staged.clone() {
                match ctx.vault_exists(&staged).await {
                    Ok(true) => {
                        out.push(staged);
                        continue;
                    }
                    Ok(false) => {} // 临时文件已不在：按来源重新落盘
                    Err(error) => {
                        return Err(format!("附件状态确认失败（{staged}）：{error}"));
                    }
                }
            }
            let bytes = ctx
                .read_binary(&image.source)
                .await
                .map_err(|error| format!("图片读取失败（{}）：{error}", image.name))?;
            let name = sanitize_name(&image.name);
            let rel = format!(".atelyx/temp/chats/{session_id}/{}-{name}", Uuid::new_v4());
            ctx.vault_write_binary(&rel, &bytes)
                .await
                .map_err(|error| format!("图片落盘失败（{rel}）：{error}"))?;
            image.staged = Some(rel.clone());
            out.push(rel);
        }
        Ok(out)
    }

    /// 手动改名：会话真源在宿主，改完重取清单（标题与排序都可能变）。
    async fn rename(&self, ctx: &Context, session_id: &str, title: &str) {
        match ctx.chat_rename(session_id, title).await {
            Ok(()) => {
                self.model.lock().unwrap().set_title(session_id, Some(title.to_string()));
                self.load_list(ctx).await;
            }
            Err(error) => self.set_error(format!("会话改名失败：{error}")),
        }
        self.render(ctx).await;
    }

    async fn send(&self, ctx: &Context, request: SendRequest) {
        if request.display.is_empty() && request.images.is_empty() {
            self.set_error("消息内容为空");
            self.render(ctx).await;
            return;
        }
        let Some((provider, model_id)) = request.pick.clone() else {
            self.set_error("未选择模型：点击输入区下方的模型按钮选择");
            self.render(ctx).await;
            return;
        };
        let agent = request.agent.clone();
        let mut created = false;
        let session_id = match request.session_id.clone() {
            Some(id) => id,
            None => {
                let id = Uuid::new_v4().to_string();
                let now = now_ms();
                let batch = CommitBatch {
                    created: vec![SessionDef {
                        id: id.clone(),
                        file: String::new(),
                        title: None,
                        agent_id: agent.clone(),
                        compaction: None,
                        created_at: now,
                        updated_at: now,
                        messages: Vec::new(),
                    }],
                    ..Default::default()
                };
                if let Err(error) = ctx.chat_session_write(&batch).await {
                    self.set_error(format!("会话创建失败：{error}"));
                    self.render(ctx).await;
                    return;
                }
                created = true;
                let mut model = self.model.lock().unwrap();
                model.active = Some(id.clone());
                model.active_title = None;
                model.active_agent_id = agent.clone();
                model.clear_messages();
                model.compaction = None;
                id
            }
        };
        let mut images = request.images;
        let paths = match self.stage_images(ctx, &session_id, &mut images).await {
            Ok(paths) => paths,
            Err(error) => {
                self.fail_send(ctx, &session_id, created, false, error, images, request.restore)
                    .await;
                return;
            }
        };
        {
            let mut model = self.model.lock().unwrap();
            if created {
                model.images.clear();
            } else {
                model.images = images.clone();
            }
            model.streaming.insert(session_id.clone());
            model.error = None;
        }
        let params = ChatSendParams {
            text: request.display.clone(),
            session_id: Some(session_id.clone()),
            agent_id: agent,
            model: Some(model_id),
            provider_id: Some(provider),
            effort: request.effort.clone(),
            images: paths.clone(),
            display_text: Some(request.display.clone()),
            refs: request
                .refs
                .iter()
                .map(|entry| ChatRef {
                    path: entry.path.clone(),
                    label: entry.label.clone(),
                    ref_kind: entry.kind.key().to_string(),
                })
                .collect(),
        };
        let keep_input = !request.restore.is_empty();
        match ctx.chat_send_with(params).await {
            Ok(result) => {
                {
                    let mut model = self.model.lock().unwrap();
                    model.streaming.insert(result.session_id.clone());
                    if !keep_input {
                        model.clear_input();
                    }
                }
                self.load_session(ctx, &result.session_id).await;
            }
            Err(error) => {
                // 超时 = 结果未知：宿主可能已受理并写了消息，不能删会话 / 补回旧消息
                self.fail_send(
                    ctx,
                    &session_id,
                    created,
                    error.is_timeout(),
                    error.to_string(),
                    images,
                    request.restore,
                )
                .await;
                return;
            }
        }
        self.load_list(ctx).await;
        self.render(ctx).await;
    }

    /// 发送未成功收场：确定失败才删新会话并补回被截断的消息；结果未知时只清在途态。
    async fn fail_send(
        &self,
        ctx: &Context,
        session_id: &str,
        created: bool,
        unknown: bool,
        error: String,
        images: Vec<PendingImage>,
        restore: Vec<Value>,
    ) {
        if created && !unknown {
            if let Err(clean) = ctx.chat_delete(session_id).await {
                ctx.log_error(&format!("收回未发送成功的会话失败（{session_id}）：{clean}"));
            }
        }
        {
            let mut model = self.model.lock().unwrap();
            model.streaming.remove(session_id);
            if created {
                // 托盘图片重新挂回：staged 路径随会话删除失效时，下次发送会按来源重读
                model.images = images;
                if !unknown {
                    model.active = None;
                    model.clear_messages();
                }
            }
        }
        if !unknown && !restore.is_empty() {
            self.restore_messages(ctx, session_id, restore).await;
            self.load_session(ctx, session_id).await;
        }
        self.set_error(if unknown {
            format!("发送结果未知（等待超时）：{error}")
        } else {
            format!("发送失败：{error}")
        });
        self.load_list(ctx).await;
        self.render(ctx).await;
    }

    async fn restore_messages(&self, ctx: &Context, session_id: &str, messages: Vec<Value>) {
        let batch = CommitBatch {
            appends: vec![AppendBatch {
                session_id: session_id.to_string(),
                messages,
            }],
            ..Default::default()
        };
        if let Err(error) = ctx.chat_session_write(&batch).await {
            ctx.log_error(&format!("补回被截断的消息失败：{error}"));
        }
    }

    async fn regenerate(&self, ctx: &Context) {
        let Some((session_id, keep, user, tail, pick, effort)) = ({
            let model = self.model.lock().unwrap();
            let Some(session_id) = model.active.clone() else {
                return;
            };
            if model.streaming.contains(&session_id) {
                return;
            }
            model.regenerate_keep(&session_id).map(|(keep, user)| {
                let tail = model.messages[keep..].to_vec();
                (session_id, keep, user, tail, model.model_pick.clone(), model.effort.clone())
            })
        }) else {
            self.set_error("没有可重新生成的回复");
            self.render(ctx).await;
            return;
        };
        let Some((provider, model_id)) = pick else {
            self.set_error("未选择模型：点击输入区下方的模型按钮选择");
            self.render(ctx).await;
            return;
        };
        let display = display_of(&user).to_string();
        let refs = refs_of(&user);
        let mut images = Vec::new();
        for image in images_of(&user) {
            if let Some(path) = image.staged.as_deref() {
                match ctx.vault_exists(path).await {
                    Ok(true) => {}
                    Ok(false) => {
                        ctx.log_error(&format!("重新生成时附件已不存在，跳过：{path}"));
                        continue;
                    }
                    Err(error) => {
                        self.set_error(format!("附件状态确认失败（{path}）：{error}"));
                        self.render(ctx).await;
                        return;
                    }
                }
            }
            images.push(image);
        }
        // 截断前确认还有内容可重发：否则先截断再报「消息为空」会丢原回复
        if display.is_empty() && images.is_empty() {
            self.set_error("原始消息没有可重发的内容");
            self.render(ctx).await;
            return;
        }
        let batch = CommitBatch {
            truncations: vec![TruncateEntry {
                session_id: session_id.clone(),
                keep_count: keep,
            }],
            ..Default::default()
        };
        if let Err(error) = ctx.chat_session_write(&batch).await {
            self.set_error(format!("重新生成前截断失败：{error}"));
            self.render(ctx).await;
            return;
        }
        let agent = self.model.lock().unwrap().active_agent();
        self.send(
            ctx,
            SendRequest {
                session_id: Some(session_id),
                display,
                refs,
                images,
                agent,
                pick: Some((provider, model_id)),
                effort,
                restore: tail,
            },
        )
        .await;
    }

    async fn rollback(&self, ctx: &Context, message_id: &str) {
        let (session_id, keep) = {
            let model = self.model.lock().unwrap();
            let Some(session_id) = model.active.clone() else {
                return;
            };
            if model.streaming.contains(&session_id) {
                return;
            }
            let Some(keep) = model.rollback_keep(message_id) else {
                return;
            };
            (session_id, keep)
        };
        let batch = CommitBatch {
            truncations: vec![TruncateEntry {
                session_id: session_id.clone(),
                keep_count: keep,
            }],
            ..Default::default()
        };
        match ctx.chat_session_write(&batch).await {
            Ok(()) => {
                self.model.lock().unwrap().rollback_confirm = None;
                self.load_session(ctx, &session_id).await;
                self.load_list(ctx).await;
            }
            Err(error) => self.set_error(format!("回到此处失败：{error}")),
        }
        self.render(ctx).await;
    }

    async fn compact(&self, ctx: &Context) {
        let Some(session_id) = self.model.lock().unwrap().active.clone() else {
            return;
        };
        {
            let mut model = self.model.lock().unwrap();
            if model.streaming.contains(&session_id) {
                return;
            }
            model.compacting = Some(session_id.clone());
            model.error = None;
        }
        self.render(ctx).await;
        let result = ctx.chat_compact(&session_id).await;
        {
            let mut model = self.model.lock().unwrap();
            model.compacting = None;
            if let Err(error) = result {
                model.error = Some(format!("压缩失败：{error}"));
            }
        }
        self.load_session(ctx, &session_id).await;
        self.load_list(ctx).await;
        self.render(ctx).await;
    }

    async fn retitle(&self, ctx: &Context) {
        let Some(session_id) = self.model.lock().unwrap().active.clone() else {
            return;
        };
        {
            let mut model = self.model.lock().unwrap();
            if model.streaming.contains(&session_id) {
                return;
            }
            model.renaming.insert(session_id.clone());
            model.error = None;
        }
        self.render(ctx).await;
        let result = ctx.chat_title(&session_id).await;
        {
            let mut model = self.model.lock().unwrap();
            model.renaming.remove(&session_id);
            match result {
                Ok(()) => {}
                Err(error) => model.error = Some(format!("重新命名失败：{error}")),
            }
        }
        self.load_list(ctx).await;
        self.render(ctx).await;
    }

    async fn delete(&self, ctx: &Context, session_id: &str) {
        match ctx.chat_delete(session_id).await {
            Ok(()) => {
                self.model.lock().unwrap().remove_session(session_id);
                self.load_list(ctx).await;
            }
            Err(error) => self.set_error(format!("删除会话失败：{error}")),
        }
        self.render(ctx).await;
    }

    async fn copy(&self, ctx: &Context, text: String) {
        if let Err(error) = ctx.clipboard_write(&text).await {
            self.set_error(format!("复制失败：{error}"));
            self.render(ctx).await;
        }
    }

    async fn stop(&self, ctx: &Context) {
        let Some(session_id) = self.model.lock().unwrap().active.clone() else {
            return;
        };
        if let Err(error) = ctx.chat_stop(&session_id).await {
            self.set_error(format!("停止失败：{error}"));
        }
        self.render(ctx).await;
    }

    async fn set_agent(&self, ctx: &Context, agent_id: String) {
        let active = self.model.lock().unwrap().active.clone();
        let batch = CommitBatch {
            metas: active
                .as_ref()
                .map(|id| MetaPatch {
                    id: id.clone(),
                    agent_id: Some(Some(agent_id.clone())),
                    ..Default::default()
                })
                .into_iter()
                .collect(),
            overrides: OverridesPatch {
                agent_override: Some(Some(agent_id.clone())),
                ..Default::default()
            },
            ..Default::default()
        };
        match ctx.chat_session_write(&batch).await {
            Ok(()) => {
                let mut model = self.model.lock().unwrap();
                model.agent_override = Some(agent_id.clone());
                if active.is_some() {
                    model.active_agent_id = Some(agent_id);
                }
                model.menu = None;
            }
            Err(error) => self.set_error(format!("Agent 选择保存失败：{error}")),
        }
        self.render(ctx).await;
    }

    async fn set_model(&self, ctx: &Context, index: usize) {
        let Some(row) = self.model.lock().unwrap().models.get(index).cloned() else {
            return;
        };
        let pick = json!({ "providerId": row.provider_id, "model": row.model });
        let batch = CommitBatch {
            overrides: OverridesPatch {
                model_override: Some(Some(pick)),
                ..Default::default()
            },
            ..Default::default()
        };
        match ctx.chat_session_write(&batch).await {
            Ok(()) => {
                let mut model = self.model.lock().unwrap();
                model.model_pick = Some((row.provider_id, row.model));
                model.menu = None;
            }
            Err(error) => self.set_error(format!("模型选择保存失败：{error}")),
        }
        self.render(ctx).await;
    }

    async fn set_effort(&self, ctx: &Context, effort: Option<String>) {
        let batch = CommitBatch {
            overrides: OverridesPatch {
                effort_override: Some(effort.clone()),
                ..Default::default()
            },
            ..Default::default()
        };
        match ctx.chat_session_write(&batch).await {
            Ok(()) => {
                let mut model = self.model.lock().unwrap();
                model.effort = effort;
                model.menu = None;
            }
            Err(error) => self.set_error(format!("推理等级保存失败：{error}")),
        }
        self.render(ctx).await;
    }

    /// `#` 候选打开时的键盘导航：↑↓ 环绕、Enter 选中、Esc 关闭。
    /// 返回真 = 这次按键已被候选消费，不再交给输入框。
    async fn handle_picker_key(&self, input: &FrameInput) -> bool {
        let FrameInput::Key { key, action: KeyAction::Press, .. } = input else {
            return false;
        };
        let Key::Named { name } = key else {
            return false;
        };
        let mut moved = None;
        let edit = {
            let mut model = self.model.lock().unwrap();
            if model.picker.is_none() {
                return false;
            }
            match name {
                NamedKey::ArrowUp => {
                    model.move_picker(-1);
                    moved = model.picker_selected();
                    None
                }
                NamedKey::ArrowDown => {
                    model.move_picker(1);
                    moved = model.picker_selected();
                    None
                }
                NamedKey::Escape => {
                    model.picker = None;
                    None
                }
                NamedKey::Enter => {
                    let Some(index) = model.picker_selected() else {
                        model.picker = None;
                        return true;
                    };
                    model.pick_ref(index).map(|(value, caret)| {
                        model.draft = value.clone();
                        (value, caret)
                    })
                }
                _ => return false,
            }
        };
        let scroll = moved.and_then(|index| {
            let model = self.model.lock().unwrap();
            model
                .picker
                .as_ref()
                .map(|picker| crate::ui::picker_scroll_offset(picker, index))
        });
        {
            let mut frame_ui = self.ui.lock().await;
            if let Some((value, caret)) = edit {
                frame_ui.set_input(DRAFT, &value, caret);
            } else if let Some(offset) = scroll {
                frame_ui.scroll_to("picker", offset);
            }
        }
        true
    }

    /// Ctrl+V 且输入框有焦点：剪贴板里是图片就转成待发送附件，返回真表示这次按键已被消费；
    /// 没有图片时交回输入框做文字粘贴。
    async fn handle_image_paste(&self, ctx: &Context, input: &FrameInput) -> bool {
        let FrameInput::Key {
            key: Key::Character { text },
            action: KeyAction::Press,
            modifiers,
            ..
        } = input
        else {
            return false;
        };
        if !modifiers.control || modifiers.meta || text != "v" {
            return false;
        }
        if self.ui.lock().await.focus_id() != Some(DRAFT) {
            return false;
        }
        let png = match ctx.clipboard_read_image_png().await {
            Ok(Some(png)) => png,
            // 剪贴板里没有图片是常态；读失败也不拦文字粘贴
            Ok(None) => return false,
            Err(error) => {
                ctx.log_error(&format!("读取剪贴板图片失败：{error}"));
                return false;
            }
        };
        let dir = match ctx.private_dir().await {
            Ok(dir) => dir,
            Err(error) => {
                self.set_error(format!("剪贴板图片暂存失败：{error}"));
                return true;
            }
        };
        let path = format!("{dir}/clipboard-{}.png", Uuid::new_v4());
        if let Err(error) = ctx.write_binary(&path, &png).await {
            self.set_error(format!("剪贴板图片暂存失败：{error}"));
            return true;
        }
        self.model
            .lock()
            .unwrap()
            .push_image("剪贴板图片".to_string(), path);
        true
    }

    async fn handle_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL {
            return;
        }
        // 菜单锚点 = 触发钮的点击点（与宿主弹层同一套锚定口径：顶边贴指针，放不下向上翻）
        if let FrameInput::PointerDown { x, y, .. }
        | FrameInput::PointerUp { x, y, .. }
        | FrameInput::DoubleClick { x, y, .. } = &params.input
        {
            self.model.lock().unwrap().pointer = Some((*x, *y));
        }
        // Esc 先关浮层（候选 / 菜单），没有浮层时才交给输入框取消编辑
        if let FrameInput::Key {
            key: Key::Named { name: NamedKey::Escape },
            action: KeyAction::Press,
            ..
        } = &params.input
        {
            let closed = {
                let mut model = self.model.lock().unwrap();
                let open = model.menu.is_some() || model.picker.is_some();
                if open {
                    model.menu = None;
                    model.menu_anchor = None;
                    model.picker = None;
                }
                open
            };
            if closed {
                self.render(ctx).await;
                return;
            }
        }
        if self.handle_picker_key(&params.input).await {
            self.render(ctx).await;
            return;
        }
        if self.handle_image_paste(ctx, &params.input).await {
            self.render(ctx).await;
            return;
        }
        let events = {
            let mut frame_ui = self.ui.lock().await;
            frame_ui
                .event(ctx, &params.input, |id| {
                    let model = self.model.lock().unwrap();
                    match id {
                        DRAFT => Some(model.draft.clone()),
                        "search" => Some(model.search.clone()),
                        _ => None,
                    }
                })
                .await
        };
        let caret = self.ui.lock().await.caret(DRAFT).map(|(caret, _)| caret);
        let mut actions = Vec::new();
        let mut edit: Option<(String, usize)> = None;
        {
            let mut model = self.model.lock().unwrap();
            for event in events {
                match event {
                    UiEvent::Click { id } => {
                        on_click(&mut model, &id, caret, &mut actions, &mut edit);
                    }
                    UiEvent::Text { id, value } if id == DRAFT => {
                        model.draft = value;
                        let caret = caret.unwrap_or(model.draft.len());
                        model.refresh_picker(caret);
                    }
                    // 只有选在动（点进标签整段选中、方向键移动）：候选浮层要跟着开合
                    UiEvent::Caret { id, caret, .. } if id == DRAFT => {
                        model.refresh_picker(caret);
                    }
                    UiEvent::Text { id, value } if id == "search" => {
                        model.search = value;
                    }
                    UiEvent::Text { id, value } if id.starts_with("rename-input:") => {
                        if let Some(rename) = model.rename.as_mut() {
                            rename.title = value;
                        }
                    }
                    UiEvent::Submit { id } if id.starts_with("rename-input:") => {
                        if let Some(rename) = model.rename.take() {
                            let title = rename.title.trim().to_string();
                            if !title.is_empty() {
                                actions.push(Action::Rename(rename.id, title));
                            }
                        }
                    }
                    UiEvent::Submit { id } if id == DRAFT => actions.push(Action::Send),
                    UiEvent::Cancel { .. } => {
                        model.menu = None;
                        model.menu_anchor = None;
                        model.picker = None;
                        model.cancel_rename();
                    }
                    UiEvent::Focus { focused: false, .. } => model.cancel_rename(),
                    UiEvent::Scroll { id, offset, max_offset } if id == crate::model::FLOW => {
                        model.follow_bottom = offset >= max_offset - 0.5;
                    }
                    _ => {}
                }
            }
        }
        if let Some((value, caret)) = edit {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.set_input(DRAFT, &value, caret);
        }
        // 候选刚展开：取一次当前打开的文件，让它们按打开顺序置顶（取不到就按原顺序）
        let stale = { self.model.lock().unwrap().open_files_stale };
        if stale {
            let paths = ctx.open_files().await.unwrap_or_default();
            let mut model = self.model.lock().unwrap();
            model.open_files = paths;
            model.open_files_stale = false;
            if let Some(caret) = model.picker.as_ref().map(|picker| picker.caret) {
                model.refresh_picker(caret);
            }
        }
        for action in actions {
            self.perform(ctx, action).await;
        }
        self.render(ctx).await;
    }

    async fn perform(&self, ctx: &Context, action: Action) {
        match action {
            Action::Attach => self.attach_image(ctx).await,
            Action::Send => {
                let request = {
                    let model = self.model.lock().unwrap();
                    SendRequest {
                        session_id: model.active.clone(),
                        display: model.draft.trim().to_string(),
                        refs: model.refs.clone(),
                        images: model.images.clone(),
                        agent: model.active_agent(),
                        pick: model.model_pick.clone(),
                        effort: model.effort.clone(),
                        restore: Vec::new(),
                    }
                };
                self.send(ctx, request).await;
            }
            Action::Regenerate => self.regenerate(ctx).await,
            Action::Rollback(id) => self.rollback(ctx, &id).await,
            Action::Stop => self.stop(ctx).await,
            Action::Compact => self.compact(ctx).await,
            Action::Retitle => self.retitle(ctx).await,
            Action::Delete(id) => self.delete(ctx, &id).await,
            Action::Rename(id, title) => self.rename(ctx, &id, &title).await,
            Action::ScrollBottom => {
                self.ui.lock().await.scroll_to_end(crate::model::FLOW);
                self.model.lock().unwrap().follow_bottom = true;
                self.render(ctx).await;
            }
            Action::Copy(text) => self.copy(ctx, text).await,
            Action::LoadSession(id) => {
                self.load_session(ctx, &id).await;
                self.render(ctx).await;
            }
            Action::SetAgent(id) => self.set_agent(ctx, id).await,
            Action::SetModel(index) => self.set_model(ctx, index).await,
            Action::SetEffort(effort) => self.set_effort(ctx, effort).await,
        }
    }
}

struct SendRequest {
    session_id: Option<String>,
    display: String,
    refs: Vec<RefEntry>,
    images: Vec<PendingImage>,
    agent: Option<String>,
    pick: Option<(String, String)>,
    effort: Option<String>,
    /// 重新生成失败时补回被截断的消息（含用户消息与其后的回复）。
    restore: Vec<Value>,
}

enum Action {
    Attach,
    Send,
    Regenerate,
    Rollback(String),
    Stop,
    Compact,
    Retitle,
    Delete(String),
    Rename(String, String),
    Copy(String),
    LoadSession(String),
    SetAgent(String),
    SetModel(usize),
    SetEffort(Option<String>),
    ScrollBottom,
}

fn on_click(
    model: &mut Model,
    id: &str,
    caret: Option<usize>,
    actions: &mut Vec<Action>,
    edit: &mut Option<(String, usize)>,
) {
    // 浮层内的空白按压由浮层兜底命中（`@overlay:*`）承接：不当作外点
    if id.starts_with("@overlay:") {
        return;
    }
    if id == "popup-backdrop" {
        model.menu = None;
        model.menu_anchor = None;
        return;
    }
    let menu_click = matches!(
        id,
        "plus-menu" | "agent-menu" | "model-menu" | "open-model" | "open-effort"
    ) || id.starts_with("agent:")
        || id.starts_with("model:")
        || id.starts_with("effort:");
    if !menu_click {
        model.menu = None;
        model.menu_anchor = None;
    }
    if !id.starts_with("pick:") {
        model.picker = None;
    }
    match id {
        "new" => {
            model.active = None;
            model.active_title = None;
            model.active_agent_id = None;
            model.clear_messages();
            model.compaction = None;
            model.rollback_confirm = None;
            model.menu = None;
            model.follow_bottom = true;
        }
        "toggle-sidebar" => model.sidebar_open = !model.sidebar_open,
        "sidebar-backdrop" => model.sidebar_open = false,
        "clear-error" => {
            model.error = None;
            model.persist_error = None;
        }
        "attach" | "plus-attach" => actions.push(Action::Attach),
        "context" | "plus-context" => {
            let caret = caret.unwrap_or(model.draft.len()).min(model.draft.len());
            let caret = model.insert_hash(caret);
            model.refresh_picker(caret);
            *edit = Some((model.draft.clone(), caret));
        }
        "send" => actions.push(Action::Send),
        "stop" => actions.push(Action::Stop),
        "scroll-bottom" => actions.push(Action::ScrollBottom),
        "compact" => actions.push(Action::Compact),
        "regen-title" => actions.push(Action::Retitle),
        "plus-menu" => {
            if model.menu == Some(model::Menu::Plus) {
                model.menu = None;
            } else {
                model.menu = Some(model::Menu::Plus);
                model.menu_anchor = model.pointer;
            }
        }
        "agent-menu" => {
            if model.menu == Some(model::Menu::Agent) {
                model.menu = None;
            } else {
                model.menu = Some(model::Menu::Agent);
                model.menu_anchor = model.pointer;
            }
        }
        "model-menu" => {
            if model.menu == Some(model::Menu::ModelRoot) {
                model.menu = None;
            } else {
                model.menu = Some(model::Menu::ModelRoot);
                model.menu_anchor = model.pointer;
            }
        }
        "open-model" => model.menu = Some(model::Menu::ModelList),
        "open-effort" => model.menu = Some(model::Menu::Effort),
        "rollback-cancel" => model.rollback_confirm = None,
        _ => {
            if let Some(rest) = id.strip_prefix("session:") {
                model.active = Some(rest.to_string());
                model.active_title = None;
                model.active_agent_id = None;
                model.clear_messages();
                model.compaction = None;
                model.error = None;
                model.rollback_confirm = None;
                model.session_menu = None;
                model.delete_confirm = None;
                model.follow_bottom = true;
                actions.push(Action::LoadSession(rest.to_string()));
            } else if let Some(rest) = id.strip_prefix("more:") {
                model.session_menu = if model.session_menu.as_deref() == Some(rest) {
                    None
                } else {
                    Some(rest.to_string())
                };
            } else if let Some(rest) = id.strip_prefix("rename:") {
                model.begin_rename(rest);
            } else if let Some(rest) = id.strip_prefix("delete:") {
                model.session_menu = None;
                model.delete_confirm = Some(rest.to_string());
            } else if let Some(rest) = id.strip_prefix("cancel-delete:") {
                if model.delete_confirm.as_deref() == Some(rest) {
                    model.delete_confirm = None;
                }
            } else if let Some(rest) = id.strip_prefix("close-menu:") {
                if model.session_menu.as_deref() == Some(rest) {
                    model.session_menu = None;
                }
            } else if let Some(rest) = id.strip_prefix("confirm:") {
                model.delete_confirm = None;
                actions.push(Action::Delete(rest.to_string()));
            } else if let Some(rest) = id.strip_prefix("copy:") {
                if let Some(text) = model.message_by_id(rest).map(display_of) {
                    actions.push(Action::Copy(text.to_string()));
                }
            } else if let Some(rest) = id.strip_prefix("rollback-confirm:") {
                model.rollback_confirm = None;
                actions.push(Action::Rollback(rest.to_string()));
            } else if let Some(rest) = id.strip_prefix("rollback:") {
                if model.rollback_confirm.as_deref() == Some(rest) {
                    model.rollback_confirm = None;
                    actions.push(Action::Rollback(rest.to_string()));
                } else {
                    model.rollback_confirm = Some(rest.to_string());
                }
            } else if let Some(rest) = id.strip_prefix("regen:") {
                if model.is_last(rest) && !model.active_streaming() {
                    actions.push(Action::Regenerate);
                }
            } else if let Some(rest) = id.strip_prefix("step:") {
                if !model.expanded.remove(rest) {
                    model.expanded.insert(rest.to_string());
                }
            } else if let Some(rest) = id.strip_prefix("remove-image:") {
                if let Ok(index) = rest.parse::<usize>() {
                    if index < model.images.len() {
                        model.images.remove(index);
                    }
                }
            } else if let Some(rest) = id.strip_prefix("pick:") {
                if let Ok(index) = rest.parse::<usize>() {
                    if let Some((value, caret_after)) = model.pick_ref(index) {
                        model.draft = value.clone();
                        *edit = Some((value, caret_after));
                    }
                }
            } else if let Some(rest) = id.strip_prefix("agent:") {
                model.menu = None;
                actions.push(Action::SetAgent(rest.to_string()));
            } else if let Some(rest) = id.strip_prefix("model:") {
                if let Ok(index) = rest.parse::<usize>() {
                    model.menu = None;
                    actions.push(Action::SetModel(index));
                }
            } else if let Some(rest) = id.strip_prefix("effort:") {
                let effort = match rest {
                    "default" => None,
                    wire => Some(wire.to_string()),
                };
                model.menu = None;
                actions.push(Action::SetEffort(effort));
            }
        }
    }
}

/// 用户消息里持久化的引用条目 → 输入态引用（未知类型丢弃）。
fn refs_of(message: &Value) -> Vec<RefEntry> {
    message
        .get("refs")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let path = entry.get("path").and_then(Value::as_str)?.to_string();
                    let label = entry
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let kind = match entry.get("refKind").and_then(Value::as_str) {
                        Some("note") => RefKind::Note,
                        Some("table") => RefKind::Table,
                        Some("canvas") => RefKind::Canvas,
                        Some("dir") => RefKind::Dir,
                        _ => return None,
                    };
                    Some(RefEntry { path, label, kind })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 用户消息里的图片附件 → 待发送图片（文件不存在时仍交给宿主判定）。
fn images_of(message: &Value) -> Vec<PendingImage> {
    message
        .get("attachments")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("kind").and_then(Value::as_str) == Some("image"))
                .filter_map(|entry| {
                    let file = entry.get("file").and_then(Value::as_str)?.to_string();
                    let name = file.rsplit('/').next().unwrap_or("image").to_string();
                    Some(PendingImage { name, source: file.clone(), staged: Some(file) })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sanitize_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "image".to_string()
    } else {
        cleaned
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[async_trait::async_trait]
impl PluginHandler for AiPanelPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "AI 对话", Some("sparkles"), None)
            .await
        {
            ctx.log_error(&format!("注册对话面板失败：{error}"));
        }
        for name in [
            atelyx_plugin_protocol::method::ai::HOST_CHAT_EVENT,
            event::name::AI_CHAT_CHANGED,
            event::name::AI_CHAT_RELOAD,
            event::name::AI_CHAT_PERSIST,
            event::name::VAULT_SWITCH,
            event::name::VAULT_FILE_CHANGED,
            event::name::WINDOW_FILE_DROP,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        self.load_choices(ctx).await;
        self.load_files(ctx).await;
        self.load_all(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != PANEL {
            return;
        }
        *self.attached.lock().unwrap() = true;
        // 重挂只重取主题，保留宿主已上报的尺寸（否则首帧按占位尺寸画、等 resize 再跳）
        self.ui.lock().await.refresh_theme();
        self.load_choices(ctx).await;
        if !self.model.lock().unwrap().files_ready {
            self.load_files(ctx).await;
        }
        self.load_all(ctx).await;
        self.render(ctx).await;
    }

    async fn on_surface_detached(&self, _ctx: &Context, params: SurfaceDetachedParams) {
        if params.surface_id == PANEL {
            *self.attached.lock().unwrap() = false;
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        match params.name.as_str() {
            event::name::VAULT_SWITCH => {
                self.model.lock().unwrap().reset();
                self.load_files(ctx).await;
                self.load_all(ctx).await;
                self.render(ctx).await;
            }
            event::name::VAULT_FILE_CHANGED => {
                let Ok(payload) =
                    serde_json::from_value::<FileChangedPayload>(params.payload.clone())
                else {
                    ctx.log_error("仓库文件变更载荷不可解析");
                    return;
                };
                // 自动保存的 updated 不改候选集合；结构变化才重扫文件树
                if payload.kind != FileChangeKind::Updated {
                    self.load_files(ctx).await;
                }
            }
            event::name::WINDOW_FILE_DROP => {
                // 面板未挂载时不接收：避免把图片塞进看不见的输入区
                if !self.is_attached() {
                    return;
                }
                let Ok(payload) = serde_json::from_value::<FileDropPayload>(params.payload.clone())
                else {
                    ctx.log_error("文件拖入载荷不可解析");
                    return;
                };
                let mut added = 0usize;
                let mut rejected = Vec::new();
                for source in payload.paths {
                    let Some(name) = std::path::Path::new(&source)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let ext = name
                        .rsplit_once('.')
                        .map(|(_, ext)| ext.to_ascii_lowercase())
                        .unwrap_or_default();
                    if !IMAGE_EXTS.contains(&ext.as_str()) {
                        rejected.push(name);
                        continue;
                    }
                    let mut model = self.model.lock().unwrap();
                    if model.images.iter().any(|image| image.source == source) {
                        continue;
                    }
                    model.push_image(name, source);
                    added += 1;
                }
                if !rejected.is_empty() {
                    self.set_error(format!(
                        "暂不支持发送该类型文件：{}（仅图片）",
                        rejected.join("、")
                    ));
                }
                if added > 0 || !rejected.is_empty() {
                    self.render(ctx).await;
                }
            }
            event::name::AI_CHAT_RELOAD => {
                // 会话真源已换：整体重取，旧展示与输入态一并作废
                self.model.lock().unwrap().reset();
                self.load_choices(ctx).await;
                self.load_files(ctx).await;
                self.load_all(ctx).await;
                self.render(ctx).await;
            }
            event::name::AI_CHAT_PERSIST => {
                let Ok(payload) =
                    serde_json::from_value::<ChatPersistChangedPayload>(params.payload.clone())
                else {
                    ctx.log_error("会话落盘状态载荷不可解析");
                    return;
                };
                let message = payload.persist_error.map(|error| error.message);
                self.model.lock().unwrap().set_persist_error(message);
                self.render(ctx).await;
            }
            event::name::AI_CHAT_CHANGED => {
                let Ok(payload) =
                    serde_json::from_value::<ChatChangedPayload>(params.payload.clone())
                else {
                    ctx.log_error("会话变更载荷不可解析");
                    return;
                };
                let active = self.model.lock().unwrap().active.clone();
                if payload.kind == ChatChangeKind::Deleted {
                    self.model.lock().unwrap().remove_session(&payload.session_id);
                } else if active.as_deref() == Some(payload.session_id.as_str()) {
                    self.load_session(ctx, &payload.session_id).await;
                }
                self.load_list(ctx).await;
                self.render(ctx).await;
            }
            _ => {}
        }
    }

    async fn on_chat_event(&self, ctx: &Context, params: ChatEventParams) {
        let known = {
            let model = self.model.lock().unwrap();
            model.active.as_deref() == Some(params.session_id.as_str())
                || model.session(&params.session_id).is_some()
                || model.streaming.contains(&params.session_id)
        };
        if !known && !matches!(&params.event, ChatEvent::Titled { .. }) {
            return;
        }
        match params.event {
            ChatEvent::Message { message, .. } => {
                let mut model = self.model.lock().unwrap();
                model.streaming.insert(params.session_id.clone());
                model.upsert_message(&params.session_id, &params.message_id, message);
            }
            ChatEvent::Finished { .. } => {
                let active = {
                    let mut model = self.model.lock().unwrap();
                    model.streaming.remove(&params.session_id);
                    model.active.as_deref() == Some(params.session_id.as_str())
                };
                if active {
                    self.load_session(ctx, &params.session_id).await;
                    self.load_list(ctx).await;
                }
            }
            ChatEvent::Failed { error } => {
                let active = {
                    let mut model = self.model.lock().unwrap();
                    model.streaming.remove(&params.session_id);
                    let active = model.active.as_deref() == Some(params.session_id.as_str());
                    if active {
                        model.error = Some(format!("回复失败：{error}"));
                    }
                    active
                };
                if active {
                    self.load_session(ctx, &params.session_id).await;
                }
            }
            ChatEvent::Removed => {
                self.model
                    .lock()
                    .unwrap()
                    .remove_message(&params.session_id, &params.message_id);
            }
            ChatEvent::Titled { title } => {
                self.model.lock().unwrap().set_title(&params.session_id, title);
            }
        }
        self.render(ctx).await;
    }

    async fn on_frame_tick(&self, ctx: &Context, _params: FrameTickParams) {
        // 控件层按拍推进转圈相位与光标闪烁；动画停了它会自己撤掉帧拍声明
        self.ui.lock().await.tick();
        self.render(ctx).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        self.handle_input(ctx, params).await;
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "AI 对话".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(AiPanelPlugin::new()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_matches_the_crate_identity() {
        let manifest: Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
    }

    #[test]
    fn regenerated_images_keep_their_vault_paths() {
        let message = json!({
            "attachments": [
                { "kind": "image", "mime": "image/png", "file": ".atelyx/temp/chats/s/a.png" }
            ]
        });
        let images = images_of(&message);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].staged.as_deref(), Some(".atelyx/temp/chats/s/a.png"));
    }
}
