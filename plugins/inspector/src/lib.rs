//! 属性面板插件：当前文档的 frontmatter 属性编辑与画布属性只读展示。
//! 正文真源、保存链与属性块拼接都在宿主，插件只持草稿并在提交时回传整块。

mod model;
mod ui;

use std::sync::Mutex;

use atelyx_plugin_protocol::event::{
    self, CanvasChangedPayload, CanvasVisiblePayload, PublishParams,
};
use atelyx_plugin_protocol::frame::{FrameInput, FrameInputParams, Key};
use atelyx_plugin_protocol::method::{
    self, CanvasSnapshotResult, ContributionSlot, DocPropsResult, SurfaceAttachedParams,
};
use atelyx_plugin_props as props;
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use crate::model::{DocProps, InspectorModel, PANEL};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.inspector";

struct InspectorPlugin {
    model: Mutex<InspectorModel>,
    ui: AsyncMutex<FrameUi>,
}

impl InspectorPlugin {
    fn new() -> Self {
        Self { model: Mutex::new(InspectorModel::default()), ui: AsyncMutex::new(FrameUi::new()) }
    }

    async fn render(&self, ctx: &Context) {
        let mut frame_ui = self.ui.lock().await;
        let result = frame_ui
            .render(ctx, PANEL, |frame| {
                let mut model = self.model.lock().unwrap();
                ui::frame(frame, &mut model);
                let pointer = model.pointer;
                ui::overlays(frame, &model, pointer);
            })
            .await;
        if let Err(error) = result {
            ctx.log_error(&format!("提交属性面失败：{error}"));
        }
    }

    /// 取当前文档的属性区与标签词表；画布数据由 `canvas:changed` 事件与挂面时的一次快照补全。
    async fn refresh(&self, ctx: &Context) {
        let doc = ctx.call_raw(method::doc::PROPS_READ, serde_json::json!({})).await;
        let tags = ctx.vault_tags().await.unwrap_or_default();
        let mut model = self.model.lock().unwrap();
        model.tags = tags;
        match doc {
            Ok(value) => match serde_json::from_value::<DocPropsResult>(value) {
                Ok(result) => {
                    if result.path.is_empty() {
                        model.clear_doc();
                    } else {
                        model.set_doc(DocProps {
                            block: result.frontmatter,
                            present: result.present,
                            parsed: result.parsed,
                            editable: result.editable,
                        });
                    }
                }
                Err(error) => ctx.log_error(&format!("属性回执不可解析：{error}")),
            },
            Err(error) => {
                ctx.log_error(&format!("读取属性失败：{error}"));
                model.clear_doc();
            }
        }
    }

    /// 挂面时取一次画布快照：只在此处调用画布服务，事件回调里不回调画布，避免与画布的事件派发互等。
    async fn refresh_canvas(&self, ctx: &Context) {
        let snapshot = ctx
            .call_raw(method::canvas::SNAPSHOT, serde_json::json!({}))
            .await
            .ok()
            .and_then(|value| serde_json::from_value::<CanvasSnapshotResult>(value).ok());
        let mut model = self.model.lock().unwrap();
        match snapshot {
            Some(snapshot) => model.set_canvas(snapshot),
            None => model.canvas = None,
        }
    }

    /// 渲染期攒下的属性块提交给宿主；宿主拼回正文后重读一次，保证草稿与磁盘口径一致。
    async fn flush_pending(&self, ctx: &Context) -> bool {
        let block = {
            let mut model = self.model.lock().unwrap();
            model.pending_write.take()
        };
        let Some(block) = block else { return false };
        let params = serde_json::json!({ "frontmatter": block });
        match ctx.call_raw(method::doc::PROPS_WRITE, params).await {
            Ok(_) => {
                self.refresh(ctx).await;
                false
            }
            Err(error) => {
                let text = format!("属性保存失败：{error}");
                self.model.lock().unwrap().props.error = Some(text.clone());
                ctx.log_error(&text);
                true
            }
        }
    }

    async fn handle_input(&self, ctx: &Context, params: FrameInputParams) {
        let input = params.input;
        self.remember_pointer(&input);
        if self.handle_props_key(ctx, &input).await {
            self.render(ctx).await;
            return;
        }
        let (events, _carets) = {
            let mut frame_ui = self.ui.lock().await;
            let events = frame_ui
                .event(ctx, &input, |id| value_of(&self.model.lock().unwrap(), id))
                .await;
            (events, ())
        };
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                UiEvent::Click { id } => self.on_click(ctx, &id).await,
                UiEvent::Text { id, value } => {
                    if id == props::KEY_INPUT {
                        let mut model = self.model.lock().unwrap();
                        model.props.suggest_query = value.clone();
                        model.props.draft = value;
                        model.props.suggest_index = 0;
                        model.props.suggest_armed = false;
                    }
                }
                UiEvent::Submit { id } => {
                    if id == props::KEY_INPUT {
                        self.submit_props(ctx).await;
                    }
                }
                UiEvent::Cancel { id } => {
                    if id == props::KEY_INPUT {
                        let mut model = self.model.lock().unwrap();
                        props::cancel_edit(&mut *model);
                    }
                }
                UiEvent::Focus { id, focused: false } => {
                    if id == props::KEY_INPUT {
                        self.commit_props(ctx, 0).await;
                    }
                }
                _ => {}
            }
        }
        self.flush_pending(ctx).await;
        self.render(ctx).await;
    }

    fn remember_pointer(&self, input: &FrameInput) {
        let position = match input {
            FrameInput::PointerDown { x, y, .. }
            | FrameInput::PointerMove { x, y, .. }
            | FrameInput::DoubleClick { x, y, .. } => Some((*x, *y)),
            _ => None,
        };
        if let Some(position) = position {
            self.model.lock().unwrap().pointer = position;
        }
    }

    async fn on_click(&self, ctx: &Context, id: &str) {
        if let Some(rest) = id.strip_prefix("props:suggest:") {
            let Ok(index) = rest.parse::<usize>() else { return };
            let next = {
                let mut model = self.model.lock().unwrap();
                let (_text, next) = props::pick_suggestion(&mut *model, index);
                next
            };
            if next {
                self.focus_input().await;
            }
            self.flush_pending(ctx).await;
            return;
        }
        if id.starts_with("props:typepick:") {
            {
                let mut model = self.model.lock().unwrap();
                let _ = props::on_type_pick(&mut *model, id);
            }
            self.flush_pending(ctx).await;
            return;
        }
        if !id.starts_with("props:") {
            let mut model = self.model.lock().unwrap();
            if model.props.type_menu.is_some() {
                model.props.type_menu = None;
            }
            return;
        }
        let quick = id.starts_with("props:delete:")
            || id.starts_with("props:delete-item:")
            || id.starts_with("props:toggle-bool:");
        let (editing, empty_key) = {
            let model = self.model.lock().unwrap();
            let empty_key = matches!(model.props.editing, Some(props::Slot::Key(_)))
                && model.props.draft.trim().is_empty();
            (model.props.editing.is_some(), empty_key)
        };
        if editing {
            if quick || empty_key {
                // 删除 / 翻转按渲染下标执行；空键名提交会删行，先丢草稿避免下标漂移
                props::cancel_edit(&mut *self.model.lock().unwrap());
            } else {
                self.commit_props(ctx, 0).await;
            }
        }
        if id == "props:add" {
            self.add_property(ctx).await;
            return;
        }
        if quick {
            let text = {
                let mut model = self.model.lock().unwrap();
                props::quick_action(&mut *model, id)
            };
            let _ = text;
            self.flush_pending(ctx).await;
            return;
        }
        let focus = props::on_click(&mut *self.model.lock().unwrap(), id);
        if focus {
            self.focus_input().await;
        }
    }

    async fn add_property(&self, ctx: &Context) {
        let (added, text) = {
            let mut model = self.model.lock().unwrap();
            let added = props::add_property(&mut *model);
            (added, props::text_with_placeholder(&mut *model))
        };
        let _ = text;
        if added {
            self.focus_input().await;
        }
        self.flush_pending(ctx).await;
    }

    /// 属性槽里的按键：↑↓ 巡航候选、Enter 提交、Tab 步进（Shift 反向）。
    async fn handle_props_key(&self, ctx: &Context, input: &FrameInput) -> bool {
        use atelyx_plugin_protocol::frame::{KeyAction, NamedKey};
        let FrameInput::Key { key, action, modifiers, .. } = input else {
            return false;
        };
        if *action != KeyAction::Press || modifiers.control || modifiers.alt || modifiers.meta {
            return false;
        }
        let focus = {
            let frame_ui = self.ui.lock().await;
            frame_ui.focus_id().map(str::to_string)
        };
        if focus.as_deref() != Some(props::KEY_INPUT) {
            return false;
        }
        if self.model.lock().unwrap().props.editing.is_none() {
            return false;
        }
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
                self.commit_props(ctx, if modifiers.shift { -1 } else { 1 }).await;
                true
            }
            _ => false,
        }
    }

    /// Enter：高亮候选优先，否则提交并步进到下一槽。
    async fn submit_props(&self, ctx: &Context) {
        let next = {
            let mut model = self.model.lock().unwrap();
            let (_text, next) = props::submit(&mut *model, 1);
            next
        };
        if next {
            self.focus_input().await;
        }
        self.flush_pending(ctx).await;
    }

    async fn commit_props(&self, ctx: &Context, dir: i32) {
        let next = {
            let mut model = self.model.lock().unwrap();
            let (_text, next) = props::commit(&mut *model, dir);
            next
        };
        if next {
            self.focus_input().await;
        }
        self.flush_pending(ctx).await;
    }

    async fn focus_input(&self) {
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
    }
}

/// 输入框当前值：属性面板只有一处编辑槽。
fn value_of(model: &InspectorModel, id: &str) -> Option<String> {
    if id == props::KEY_INPUT {
        return Some(model.props.draft.clone());
    }
    None
}

#[async_trait::async_trait]
impl PluginHandler for InspectorPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "属性", Some("info"), None)
            .await
        {
            ctx.log_error(&format!("注册属性面板失败：{error}"));
        }
        for name in [
            event::name::DOC_CHANGED,
            event::name::CANVAS_CHANGED,
            event::name::CANVAS_VISIBLE,
            event::name::VAULT_SWITCH,
            event::name::VAULT_FILE_CHANGED,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        self.refresh(ctx).await;
        self.render(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != PANEL {
            return;
        }
        self.ui.lock().await.refresh_theme();
        self.refresh(ctx).await;
        if self.model.lock().unwrap().canvas_visible {
            self.refresh_canvas(ctx).await;
        }
        self.render(ctx).await;
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        if params.name == event::name::CANVAS_CHANGED {
            if let Ok(payload) =
                serde_json::from_value::<CanvasChangedPayload>(params.payload.clone())
            {
                apply_canvas(&self.model, payload);
                self.render(ctx).await;
                return;
            }
        }
        if params.name == event::name::CANVAS_VISIBLE {
            if let Ok(payload) =
                serde_json::from_value::<CanvasVisiblePayload>(params.payload)
            {
                self.model.lock().unwrap().canvas_visible = payload.visible;
                self.render(ctx).await;
                return;
            }
        }
        self.refresh(ctx).await;
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
            name: "属性".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(InspectorPlugin::new()),
    )
}

/// 画布事件自带属性快照：直接采用，事件回调里不再回调画布服务。
fn apply_canvas(model: &std::sync::Mutex<InspectorModel>, payload: CanvasChangedPayload) {
    let mut model = model.lock().unwrap();
    model.canvas = payload.path.map(|path| CanvasSnapshotResult {
        path: Some(path),
        dirty: payload.dirty,
        selection: payload.selection,
        stats: payload.stats,
    });
}
