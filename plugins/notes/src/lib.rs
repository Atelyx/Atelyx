//! 笔记插件：只读 / 预览编辑 / 源码三档。正文真源、保存链与协作增量都在宿主，
//! 插件只持本端副本与编辑态。

mod model;
mod ui;

use std::sync::Mutex;

use atelyx_plugin_protocol::event::{
    self, FileChangeKind, FileChangedPayload, NoteChangedPayload, PublishParams,
};
use atelyx_plugin_protocol::frame::{FrameInput, FrameInputParams, FrameTickParams, Key, KeyAction};
use atelyx_plugin_protocol::method::{
    ContributionSlot, DocCloseParams, DocOpenParams, SurfaceAttachedParams,
};
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use crate::model::{Backlink, Mode, NoteModel, PANEL, SOURCE};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.notes";

struct NotesPlugin {
    model: Mutex<NoteModel>,
    ui: AsyncMutex<FrameUi>,
}

impl NotesPlugin {
    fn new() -> Self {
        Self { model: Mutex::new(NoteModel::default()), ui: AsyncMutex::new(FrameUi::new()) }
    }

    async fn render(&self, ctx: &Context) {
        let snapshot = self.model.lock().unwrap().clone_for_view();
        let mut frame_ui = self.ui.lock().await;
        if let Err(error) = frame_ui.render(ctx, PANEL, |frame| ui::frame(frame, &snapshot)).await {
            ctx.log_error(&format!("提交笔记面失败：{error}"));
        }
    }

    async fn load(&self, ctx: &Context, path: &str) {
        match ctx.note_open(path).await {
            Ok(result) => {
                {
                    let mut model = self.model.lock().unwrap();
                    model.load(result.path.clone(), result.text, result.can_write);
                    model.dirty = result.dirty;
                    model.save_error = result.save_error;
                }
                self.load_backlinks(ctx, &result.path).await;
            }
            Err(error) => {
                ctx.log_error(&format!("打开笔记失败（{path}）：{error}"));
                // 打开失败不得沿用上一篇正文（下一次编辑会被提交到新路径）。
                let mut model = self.model.lock().unwrap();
                model.clear();
                model.open_error = Some(error.to_string());
            }
        }
        self.render(ctx).await;
    }

    async fn load_backlinks(&self, ctx: &Context, path: &str) {
        match ctx.note_backlinks(path).await {
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

    /// 一次本地编辑：先写模型再提交宿主（宿主合并协作远端内容后回发 `note:changed`）。
    async fn commit_edit(&self, ctx: &Context, text: String, selection: Option<(usize, usize)>) {
        let path = {
            let mut model = self.model.lock().unwrap();
            if model.text != text {
                model.edit_text(text.clone());
            }
            model.path.clone()
        };
        let Some(path) = path else { return };
        let (anchor, head) = selection.unwrap_or((text.len(), text.len()));
        if let Err(error) = ctx.note_edit(&path, &text, Some(anchor), Some(head)).await {
            ctx.log_error(&format!("提交笔记编辑失败（{path}）：{error}"));
            // 宿主拒绝（超限 / 只读身份 / 会话已换）时必须可见：否则界面一直停在「未保存」。
            self.model.lock().unwrap().save_error = Some(error.to_string());
        }
    }

    async fn save(&self, ctx: &Context, manual: bool) {
        let path = self.model.lock().unwrap().path.clone();
        let Some(path) = path else { return };
        if let Err(error) = ctx.note_save(&path, manual).await {
            ctx.log_error(&format!("请求保存失败（{path}）：{error}"));
            self.model.lock().unwrap().save_error = Some(error.to_string());
        }
    }

    async fn notify_caret(&self, ctx: &Context, selection: Option<(usize, usize)>) {
        let path = self.model.lock().unwrap().path.clone();
        let Some(path) = path else { return };
        let (anchor, head) = selection.unwrap_or((0, 0));
        if selection.is_some() {
            if let Err(error) = ctx.note_awareness(&path, Some(anchor), Some(head)).await {
                ctx.log_error(&format!("上报笔记选区失败（{path}）：{error}"));
            }
        }
    }

    async fn undo(&self, ctx: &Context, redo: bool) {
        let (text, input) = {
            let mut model = self.model.lock().unwrap();
            let Some(text) = (if redo { model.redo() } else { model.undo() }) else {
                return;
            };
            let input = active_input(&model)
                .and_then(|id| value_of(&model, &id).map(|value| (id, value)));
            (text, input)
        };
        if let Some((id, value)) = input {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.set_input(&id, &value, value.len());
        }
        self.commit_edit(ctx, text, None).await;
        self.render(ctx).await;
    }

    async fn handle_input(&self, ctx: &Context, params: FrameInputParams) {
        let editable = {
            let model = self.model.lock().unwrap();
            model.mode != Mode::Read && model.can_write
        };
        if editable {
            if let Some(redo) = undo_key(&params.input) {
                self.undo(ctx, redo).await;
                return;
            }
        }
        if save_key(&params.input) {
            // 未改动不写盘：空跑一次保存会多出一条历史版本。
            if self.model.lock().unwrap().dirty {
                self.save(ctx, true).await;
                self.render(ctx).await;
            }
            return;
        }
        let (events, carets) = {
            let values = self.model.lock().unwrap().clone_for_view();
            let mut frame_ui = self.ui.lock().await;
            let events = frame_ui
                .event(ctx, &params.input, |id| value_of(&values, id))
                .await;
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
                UiEvent::DoubleClick { id } => self.enter_block(ctx, &id).await,
                UiEvent::Text { id, value } => {
                    let caret = carets.next().and_then(|(_, caret)| caret);
                    self.on_text(ctx, &id, value, caret).await;
                }
                UiEvent::Caret { id, caret, anchor } => {
                    let selection = selection_of(&self.model.lock().unwrap(), &id, caret, anchor);
                    self.notify_caret(ctx, selection).await;
                }
                UiEvent::Cancel { .. } => {
                    self.model.lock().unwrap().active_block = None;
                }
                _ => {}
            }
        }
        self.render(ctx).await;
    }

    async fn on_click(&self, ctx: &Context, id: &str) {
        match id {
            "note:save" => self.save(ctx, true).await,
            "note:history" => {
                let path = self.model.lock().unwrap().path.clone();
                if let Some(path) = path {
                    if let Err(error) = ctx.history_open("note", &path).await {
                        ctx.log_error(&format!("打开历史失败（{path}）：{error}"));
                    }
                }
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
                    }
                }
            }
        }
    }

    /// 双击预览块：把该块揭示成源码编辑面，光标落在块首。
    async fn enter_block(&self, ctx: &Context, id: &str) {
        let (block, value) = {
            let mut model = self.model.lock().unwrap();
            if model.mode != Mode::Edit || !model.can_write {
                return;
            }
            let (block, value) = if id == ui::EMPTY_EDIT {
                (None, model.text[model.body_start.min(model.text.len())..].to_string())
            } else {
                let Some(index) = block_index(id) else { return };
                if model.active_block == Some(index) {
                    // 已在编辑本块：双击只做选词（控件层已处理），不重置光标。
                    return;
                }
                let Some((from, to)) = model.block_source(index) else { return };
                (Some(index), model.text[from..to].to_string())
            };
            model.active_block = block;
            (block, value)
        };
        let mut frame_ui = self.ui.lock().await;
        let input = match block {
            Some(index) => ui::block_id(index),
            None => ui::EMPTY_EDIT.to_string(),
        };
        frame_ui.focus_input(&input, &value);
        frame_ui.set_input(&input, &value, 0);
        drop(frame_ui);
        self.render(ctx).await;
    }

    async fn on_text(
        &self,
        ctx: &Context,
        id: &str,
        value: String,
        caret: Option<(usize, usize)>,
    ) {
        let value_len = value.len();
        let (text, selection, handoff) = {
            let mut model = self.model.lock().unwrap();
            if model.mode == Mode::Read || !model.can_write {
                return;
            }
            let next = if id == SOURCE {
                value.clone()
            } else if id == ui::EMPTY_EDIT {
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
            model.edit_text(next.clone());
            let (caret, anchor) = caret.unwrap_or((value_len, value_len));
            let selection = selection_of(&model, id, caret, anchor);
            // 空笔记敲出第一个块后，编辑面 id 从 `EMPTY_EDIT` 换成块 0：把焦点一并交给它，
            // 否则下一次渲染输入框被 prune，后续按键无处可去。
            let handoff = (id == ui::EMPTY_EDIT && !model.blocks.is_empty())
                .then(|| {
                    let block = ui::block_id(0);
                    value_of(&model, &block).map(|value| (block, value, caret))
                })
                .flatten();
            (next, selection, handoff)
        };
        if let Some((id, value, caret)) = handoff {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.focus_input(&id, &value);
            frame_ui.set_input(&id, &value, caret);
        }
        self.commit_edit(ctx, text, selection).await;
    }
}

impl NoteModel {
    /// 渲染闭包只读快照：避免渲染期间与输入处理争锁；撤销栈不需要进快照。
    fn clone_for_view(&self) -> NoteModel {
        NoteModel { undo: Vec::new(), redo: Vec::new(), ..self.clone() }
    }
}

fn active_input(model: &NoteModel) -> Option<String> {
    match model.mode {
        Mode::Source => Some(SOURCE.to_string()),
        Mode::Edit => match model.active_block {
            Some(index) => Some(ui::block_id(index)),
            None if model.blocks.is_empty() => Some(ui::EMPTY_EDIT.to_string()),
            None => None,
        },
        Mode::Read => None,
    }
}

fn block_index(id: &str) -> Option<usize> {
    id.strip_prefix("note:block:")?.parse().ok()
}

fn value_of(model: &NoteModel, id: &str) -> Option<String> {
    if id == SOURCE {
        return Some(model.text.clone());
    }
    if id == ui::EMPTY_EDIT {
        return Some(model.text[model.body_start.min(model.text.len())..].to_string());
    }
    let index = block_index(id)?;
    let (from, to) = model.block_source(index)?;
    Some(model.text[from..to].to_string())
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
    let base = if id == ui::EMPTY_EDIT {
        model.body_start.min(model.text.len())
    } else {
        model.block_source(block_index(id)?)?.0
    };
    let value_len = value_of(model, id).map(|value| value.len()).unwrap_or(0);
    let clamp = |offset: usize| base + offset.min(value_len);
    Some((clamp(anchor), clamp(caret)))
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
        for name in [
            event::name::NOTE_CHANGED,
            event::name::VAULT_SWITCH,
            event::name::VAULT_FILE_CHANGED,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        self.render(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != PANEL {
            return;
        }
        self.ui.lock().await.refresh_theme();
        self.render(ctx).await;
    }

    async fn on_doc_open(&self, ctx: &Context, params: DocOpenParams) {
        if params.surface_id != PANEL {
            return;
        }
        self.ui.lock().await.refresh_theme();
        self.load(ctx, &params.path).await;
    }

    async fn on_doc_close(&self, ctx: &Context, params: DocCloseParams) {
        if params.surface_id != PANEL {
            return;
        }
        let path = self.model.lock().unwrap().path.clone();
        if path.as_deref() == Some(params.path.as_str()) {
            if let Err(error) = ctx.note_close(&params.path).await {
                ctx.log_error(&format!("关闭笔记失败（{}）：{error}", params.path));
            }
            self.model.lock().unwrap().clear();
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        match params.name.as_str() {
            event::name::NOTE_CHANGED => {
                let Ok(payload) = serde_json::from_value::<NoteChangedPayload>(params.payload) else {
                    ctx.log_error("笔记状态载荷不可解析");
                    return;
                };
                let changed = {
                    let mut model = self.model.lock().unwrap();
                    if model.path.as_deref() != Some(payload.path.as_str()) {
                        return;
                    }
                    let changed = payload.text.is_some();
                    if let Some(text) = payload.text {
                        model.replace_text(text);
                    }
                    model.dirty = payload.dirty;
                    model.save_error = payload.save_error;
                    changed
                };
                if changed {
                    self.reset_inputs().await;
                }
                self.render(ctx).await;
            }
            event::name::VAULT_SWITCH => {
                self.model.lock().unwrap().clear();
                self.render(ctx).await;
            }
            event::name::VAULT_FILE_CHANGED => {
                let Ok(payload) = serde_json::from_value::<FileChangedPayload>(params.payload) else {
                    ctx.log_error("仓库文件变更载荷不可解析");
                    return;
                };
                let path = self.model.lock().unwrap().path.clone();
                match (payload.kind, path) {
                    (FileChangeKind::Renamed, Some(path)) => {
                        if payload.old_path.as_deref() == Some(path.as_str()) {
                            // 改名后宿主会话与文档实例已换路：重开一次取内存真源并更新本地面路径。
                            self.load(ctx, &payload.path).await;
                        }
                    }
                    (FileChangeKind::Deleted, Some(path)) if payload.path == path => {
                        self.model.lock().unwrap().clear();
                        self.render(ctx).await;
                    }
                    (_, Some(path)) => {
                        self.load_backlinks(ctx, &path).await;
                        self.render(ctx).await;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    async fn on_frame_tick(&self, ctx: &Context, _params: FrameTickParams) {
        self.ui.lock().await.tick();
        self.render(ctx).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        self.handle_input(ctx, params).await;
    }
}

impl NotesPlugin {
    /// 宿主改写正文后把输入面光标夹回文本长度（控件层每帧仍读插件给出的值）。
    async fn reset_inputs(&self) {
        let model = self.model.lock().unwrap().clone_for_view();
        let mut frame_ui = self.ui.lock().await;
        if let Some(id) = active_input(&model) {
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

    fn key(text: &str, modifiers: atelyx_plugin_protocol::frame::Modifiers) -> FrameInput {
        FrameInput::Key {
            key: Key::Character { text: text.into() },
            action: KeyAction::Press,
            modifiers,
            text: Some(text.into()),
        }
    }

    #[test]
    fn undo_shortcuts_only_fire_with_command_modifier() {
        use atelyx_plugin_protocol::frame::Modifiers;
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
    fn block_selection_maps_to_absolute_offsets() {
        let mut model = NoteModel::default();
        model.load("a.md".into(), "---\nt: x\n---\n# 标题\n\n段落\n".into(), true);
        let base = model.block_source(1).expect("段落块").0;
        let (anchor, head) = selection_of(&model, &ui::block_id(1), 2, 5).expect("选区");
        assert_eq!((anchor, head), (base + 5, base + 2), "anchor / head 保持方向");
    }

    #[test]
    fn manifest_matches_the_crate_identity() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
        assert_eq!(manifest["docTypeRegistrations"][0]["extension"], "md");
        assert_eq!(manifest["docTypeRegistrations"][0]["surface"], PANEL);
    }
}
