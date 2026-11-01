//! 笔记插件：只读 / 预览编辑 / 源码三档。正文真源、保存链与协作增量都在宿主，
//! 插件只持本端副本与编辑态。

mod edit;
mod model;
mod props;
mod ui;

use std::sync::Mutex;

use atelyx_plugin_protocol::event::{
    self, FileChangeKind, FileChangedPayload, NoteChangedPayload, NotePresencePayload,
    PublishParams,
};
use atelyx_plugin_protocol::frame::{FrameInput, FrameInputParams, FrameTickParams, Key, KeyAction};
use atelyx_plugin_protocol::method::{
    ContributionSlot, DocCloseParams, DocOpenParams, EditorPrefs, NotificationLevel,
    SurfaceAttachedParams,
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
        let mode = self.model.lock().unwrap().mode;
        match ctx.note_open(path).await {
            Ok(result) => {
                {
                    let mut model = self.model.lock().unwrap();
                    let same = model.path.as_deref() == Some(result.path.as_str());
                    model.load(result.path.clone(), result.text, result.can_write);
                    if same {
                        // 同一篇改名后重开：档位不因换路径而回落
                        model.mode = mode;
                    }
                    model.dirty = result.dirty;
                    model.save_error = result.save_error;
                }
                self.load_backlinks(ctx, &result.path).await;
                self.refresh_tags(ctx).await;
            }
            Err(error) => {
                ctx.log_error(&format!("打开笔记失败（{path}）：{error}"));
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
        let Some((anchor, head)) = selection else { return };
        if let Err(error) = ctx.note_awareness(&path, Some(anchor), Some(head)).await {
            ctx.log_error(&format!("上报笔记选区失败（{path}）：{error}"));
        }
    }

    async fn undo(&self, ctx: &Context, redo: bool) {
        let (text, input) = {
            let mut model = self.model.lock().unwrap();
            let Some(text) = (if redo { model.redo() } else { model.undo() }) else {
                return;
            };
            let input = active_input(&model).and_then(|id| {
                let value = value_of(&model, &id)?;
                let caret = if let Some((block, item)) = item_index(&id) {
                    model.item_source(block, item).map_or(value.len(), |(from, to)| to - from)
                } else {
                    block_index(&id)
                        .and_then(|index| model.blocks.get(index))
                        .map_or(value.len(), |block| block.to.saturating_sub(block.from))
                };
                Some((id, value, caret))
            });
            (text, input)
        };
        if let Some((id, value, caret)) = input {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.set_input(&id, &value, caret);
        }
        self.commit_edit(ctx, text, None).await;
        self.render(ctx).await;
    }

    async fn handle_input(&self, ctx: &Context, params: FrameInputParams) {
        let input = params.input;
        self.remember_pointer(&input);
        if let FrameInput::DoubleClick { hit, .. } = &input {
            self.on_pointer_double(ctx, hit.as_deref()).await;
        }
        if let FrameInput::PointerDown { hit, .. } = &input {
            self.on_pointer_down(ctx, hit.as_deref()).await;
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
                self.save(ctx, true).await;
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
            let events = frame_ui.event(ctx, &input, |id| value_of(&values, id)).await;
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
                    self.notify_caret(ctx, selection).await;
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
        if let Some((block, item)) = item_index(hit) {
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
        if let Some(index) = block_index(hit) {
            let active = {
                let mut model = self.model.lock().unwrap();
                // 列表块只按项揭示：块 id 不整块回显源码
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
            return;
        }
        if gap_input(hit).is_some() {
            // 空行自己托管光标：不揭示相邻块，交给控件层把光标落到空行上
            let mut model = self.model.lock().unwrap();
            model.active_block = None;
            model.active_item = None;
        }
    }

    /// 只读档双击正文：进入预览编辑并揭示该块（列表块只切模式，再按项揭示）。
    async fn on_pointer_double(&self, ctx: &Context, hit: Option<&str>) {
        let Some(index) = hit.and_then(block_index) else { return };
        let list = {
            let mut model = self.model.lock().unwrap();
            if model.mode != Mode::Read || !model.can_write {
                return;
            }
            model.mode = Mode::Edit;
            let list = model.blocks.get(index).is_some_and(|block| !block.items.is_empty());
            model.active_block = if list { None } else { Some(index) };
            model.active_item = None;
            list
        };
        if list {
            self.render(ctx).await;
        } else {
            self.focus_block(ctx, index, None).await;
        }
    }

    /// 聚焦一个块输入面：`caret` 缺省落块内容尾（源码区间还含块后空行，不能取 `value.len()`）。
    /// 列表块没有整块编辑面：转到首项（`usize::MAX` = 末项）。
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
        let caret = match caret {
            Some(usize::MAX) | None => content_end,
            Some(caret) => caret.min(content_end),
        };
        let mut frame_ui = self.ui.lock().await;
        frame_ui.focus_input(&id, &value);
        frame_ui.set_input(&id, &value, caret);
        drop(frame_ui);
        self.render(ctx).await;
    }

    /// 聚焦一个列表项输入面：`caret` 缺省落项尾。
    async fn focus_item(&self, ctx: &Context, block: usize, item: usize, caret: Option<usize>) {
        let (value, caret) = {
            let model = self.model.lock().unwrap();
            let Some((from, to)) = model.item_source(block, item) else { return };
            let value = model.text[from..to].to_string();
            let caret = match caret {
                Some(usize::MAX) | None => value.len(),
                Some(caret) => caret.min(value.len()),
            };
            (value, caret)
        };
        let id = ui::item_id(block, item);
        {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.focus_input(&id, &value);
            frame_ui.set_input(&id, &value, caret);
        }
        self.render(ctx).await;
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
            } else if let Some(index) = model.block_at(offset) {
                if !model.blocks[index].items.is_empty() {
                    // 列表块内但落在项间空行：落到最近一项（前项末尾 / 首项开头）
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
        if let Some((gap, line)) = gap_input(&focus) {
            return self.handle_gap_key(ctx, &focus, gap, line, key).await;
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
        if caret != anchor {
            return false;
        }
        let Some(source) = self.model.lock().unwrap().active_source().map(str::to_string) else {
            return false;
        };
        // 块输入面首行 / 末行的 ↑↓ 跨出输入面：先进相邻空行，再进相邻块
        if let Key::Named { name: NamedKey::ArrowUp | NamedKey::ArrowDown } = key {
            let up = matches!(key, Key::Named { name: NamedKey::ArrowUp });
            let cross = if up { caret == 0 } else { caret == source.len() };
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
            Key::Named { name: NamedKey::Enter } => edit::list_enter_edit(&source, caret),
            Key::Named { name: NamedKey::Backspace } => edit::list_backspace_edit(&source, caret),
            Key::Named { name: NamedKey::Tab } if modifiers.shift => {
                edit::outdent_edit(&source, caret, caret).map(|edit| (edit.text, edit.caret))
            }
            Key::Named { name: NamedKey::Tab } => {
                let edit = edit::indent_edit(&source, caret, caret);
                Some((edit.text, edit.caret))
            }
            _ => None,
        };
        let Some((source, caret)) = change else { return false };
        let text = {
            let mut model = self.model.lock().unwrap();
            let Some(text) = model.replace_block(index, &source) else { return false };
            model.edit_text(text.clone());
            text
        };
        {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.set_input(&id, &source, caret);
        }
        self.commit_edit(ctx, text, None).await;
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
                    let moved = props::move_suggestion(&mut model, delta);
                    (moved, props::suggest_scroll_offset(&model))
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
                self.model.lock().unwrap().edit_text(text.clone());
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
                self.model.lock().unwrap().edit_text(text.clone());
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
        if caret != anchor {
            return false;
        }
        let Some((from, to)) = self.model.lock().unwrap().item_source(block, item) else {
            return false;
        };
        let Some(source) = self.model.lock().unwrap().text.get(from..to).map(str::to_string) else {
            return false;
        };
        // 项首 / 项尾的 ↑↓ 跨出：先在列表内换项，再进相邻块
        if let Key::Named { name: NamedKey::ArrowUp | NamedKey::ArrowDown } = key {
            let up = matches!(key, Key::Named { name: NamedKey::ArrowUp });
            let cross = if up { caret == 0 } else { caret == source.len() };
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
            Key::Named { name: NamedKey::Enter } => edit::list_enter_edit(&source, caret),
            Key::Named { name: NamedKey::Backspace } => edit::list_backspace_edit(&source, caret),
            Key::Named { name: NamedKey::Tab } if shift => {
                edit::outdent_edit(&source, caret, caret).map(|edit| (edit.text, edit.caret))
            }
            Key::Named { name: NamedKey::Tab } => {
                let edit = edit::indent_edit(&source, caret, caret);
                Some((edit.text, edit.caret))
            }
            _ => None,
        };
        let Some((source, caret)) = change else { return false };
        let text = {
            let mut model = self.model.lock().unwrap();
            let Some(text) = model.replace_item(block, item, &source) else { return false };
            model.edit_text(text.clone());
            text
        };
        let at = from + caret;
        self.commit_edit(ctx, text, Some((at, at))).await;
        self.focus_at(ctx, at).await;
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
        if self.props_click(ctx, id).await {
            return;
        }
        match id {
            "note:retry" => {
                self.model.lock().unwrap().save_error = None;
                self.save(ctx, true).await;
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
                if mode == Mode::Source {
                    model.props.editing = None;
                    model.props.draft.clear();
                    props::reset_suggest(&mut model);
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

    async fn on_double_click(&self, ctx: &Context, id: &str) {
        let Some(index) = block_index(id) else { return };
        let mode = self.model.lock().unwrap().mode;
        if mode == Mode::Read {
            self.on_pointer_double(ctx, Some(id)).await;
        } else if mode == Mode::Edit {
            {
                let model = self.model.lock().unwrap();
                // 列表块只按项揭示，块 id 双击不整块回显源码
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
    }

    /// 属性面板的点击：命中即处理并返回 true。
    async fn props_click(&self, ctx: &Context, id: &str) -> bool {
        if id.starts_with("props:typepick:") {
            let text = props::on_type_pick(&mut self.model.lock().unwrap(), id);
            if let Some(text) = text {
                self.commit_edit(ctx, text, None).await;
            }
            return true;
        }
        if let Some(rest) = id.strip_prefix("props:suggest:") {
            let Ok(index) = rest.parse::<usize>() else { return true };
            let (text, next) = {
                let mut model = self.model.lock().unwrap();
                props::pick_suggestion(&mut model, index)
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
                props::cancel_edit(&mut self.model.lock().unwrap());
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
        let focus = props::on_click(&mut self.model.lock().unwrap(), id);
        if focus {
            self.focus_props_input().await;
        }
        true
    }

    async fn props_quick_action(&self, ctx: &Context, id: &str) -> bool {
        let text = {
            let mut model = self.model.lock().unwrap();
            props::quick_action(&mut model, id)
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
            props::submit(&mut model, 1)
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
            props::commit(&mut model, dir)
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
            if props::add_property(&mut model) {
                (true, props::text_with_placeholder(&model))
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
        if id == props::KEY_INPUT {
            let mut model = self.model.lock().unwrap();
            model.props.editing = None;
            model.props.draft.clear();
            model.props.error = None;
            props::reset_suggest(&mut model);
            // 丢弃未提交的草稿；已写进正文的占位属性保留
            model.refresh_props();
        } else if id == crate::model::TITLE_INPUT {
            self.model.lock().unwrap().renaming = None;
        }
    }

    async fn on_blur(&self, ctx: &Context, id: &str) {
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
            self.save(ctx, false).await;
            return;
        }
        if let Err(error) = ctx.note_rename(&path, &name).await {
            ctx.log_error(&format!("改名失败（{path}）：{error}"));
            let _ = ctx.notify("改名失败", Some(&error.to_string()), NotificationLevel::Error).await;
        }
    }

    async fn on_text(&self, ctx: &Context, id: &str, value: String, caret: Option<(usize, usize)>) {
        if id == props::KEY_INPUT {
            let mut model = self.model.lock().unwrap();
            model.props.suggest_query = value.clone();
            model.props.draft = value;
            model.props.suggest_index = 0;
            model.props.suggest_armed = false;
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
            } else {
                item_index(id).and_then(|(block, item)| model.item_source(block, item))
            }
        };
        if let Some((from, to)) = range {
            let (text, at) = {
                let mut model = self.model.lock().unwrap();
                if model.mode != Mode::Edit || !model.can_write {
                    return;
                }
                let caret = caret.map_or(value.len(), |(caret, _)| caret);
                let (text, at) = model.replace_range(from, to, &value, caret);
                model.edit_text(text.clone());
                (text, at)
            };
            self.commit_edit(ctx, text, Some((at, at))).await;
            self.focus_at(ctx, at).await;
            return;
        }
        let value_len = value.len();
        let (text, selection, handoff) = {
            let mut model = self.model.lock().unwrap();
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
            model.edit_text(next.clone());
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
        if let Some((id, value, caret)) = handoff {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.focus_input(&id, &value);
            frame_ui.set_input(&id, &value, caret);
        }
        self.commit_edit(ctx, text, selection).await;
    }
}

impl NoteModel {
    /// 渲染闭包只读快照：避免渲染期间与输入处理争锁；撤销栈不参与渲染。
    fn clone_for_view(&self) -> NoteModel {
        NoteModel {
            undo: Vec::new(),
            redo: Vec::new(),
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

/// 纵向巡航的目标（↑↓ 跨输入面时用）。
enum Focus {
    /// 块序号 + 光标（`usize::MAX` = 块尾）。
    Block(usize, usize),
    /// 列表块序 + 项序 + 光标（`usize::MAX` = 项尾）。
    Item(usize, usize, usize),
    /// 空行区序 + 行序 + 光标。
    Gap(usize, usize, usize),
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
    if let Some((block, item)) = item_index(id) {
        let (from, to) = model.item_source(block, item)?;
        return Some(model.text[from..to].to_string());
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
    let base = if id == crate::model::EMPTY_EDIT {
        model.body_start.min(model.text.len())
    } else if let Some((gap, line)) = gap_input(id) {
        model.gap_line_range(gap, line)?.0
    } else if let Some((block, item)) = item_index(id) {
        model.item_source(block, item)?.0
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
            event::name::NOTE_PRESENCE,
            event::name::EDITOR_PREFS_CHANGED,
            event::name::VAULT_SWITCH,
            event::name::VAULT_FILE_CHANGED,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        self.refresh_prefs(ctx).await;
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
                        // 远端整篇换文会让槽位下标失效：丢弃在途槽编辑，避免提交到别的属性上
                        model.props.editing = None;
                        model.props.draft.clear();
                        props::reset_suggest(&mut model);
                    }
                    model.dirty = payload.dirty;
                    model.save_error = payload.save_error;
                    changed
                };
                if changed {
                    self.reset_inputs().await;
                }
                let (pending, dirty, path) = {
                    let mut model = self.model.lock().unwrap();
                    let pending = if model.dirty { None } else { model.pending_rename.take() };
                    (pending, model.dirty, model.path.clone())
                };
                if let (Some(name), false) = (pending, dirty) {
                    if let Some(path) = path {
                        if let Err(error) = ctx.note_rename(&path, &name).await {
                            ctx.log_error(&format!("改名失败（{path}）：{error}"));
                            let _ = ctx
                                .notify("改名失败", Some(&error.to_string()), NotificationLevel::Error)
                                .await;
                        }
                    }
                }
                self.render(ctx).await;
            }
            event::name::NOTE_PRESENCE => {
                let Ok(payload) = serde_json::from_value::<NotePresencePayload>(params.payload) else {
                    ctx.log_error("笔记在场载荷不可解析");
                    return;
                };
                let next: Vec<crate::model::RemoteCursor> = payload
                    .cursors
                    .into_iter()
                    .map(|cursor| crate::model::RemoteCursor {
                        name: cursor.name,
                        color: cursor.color,
                        anchor: cursor.anchor,
                        head: cursor.head,
                    })
                    .collect();
                let changed = {
                    let mut model = self.model.lock().unwrap();
                    if payload.path.as_deref() != model.path.as_deref() {
                        return;
                    }
                    if model.remote == next {
                        return;
                    }
                    model.remote = next;
                    true
                };
                if changed {
                    self.render(ctx).await;
                }
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
                            // 改名后宿主会话与文档实例已换路：先记下新路径，重开时按「同一篇」保留档位。
                            self.model.lock().unwrap().path = Some(payload.path.clone());
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
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
        assert_eq!(manifest["docTypeRegistrations"][0]["extension"], "md");
        assert_eq!(manifest["docTypeRegistrations"][0]["surface"], PANEL);
    }
}
