//! 非激活笔记的会话停放：切走的笔记整份编辑态移入 `App.notes`，切回再装回活动字段。
//! 编辑与渲染路径只读写 App 的活动字段；本模块只负责搬家与派生缓存复位。

use std::collections::HashSet;

use iced::widget::text_editor;

use crate::markdown_edit::EditorState;
use crate::{App, NoteMode};

/// 停放中的笔记会话（撤销栈随 `edit` 一起保留）。
pub struct NoteSession {
    text: String,
    base: String,
    source_editor: text_editor::Content,
    edit: EditorState,
    note_mode: NoteMode,
    dirty: bool,
    save_error: Option<String>,
    load_error: Option<String>,
    code_expanded: HashSet<usize>,
}

impl NoteSession {
    /// 取走活动会话并清空活动字段（无打开笔记 = None）。调用前必须已完成落盘 gate。
    pub fn take(app: &mut App) -> Option<Self> {
        app.ui.last_note_file.as_ref()?;
        let text = std::mem::take(&mut app.note_text);
        let base = std::mem::take(&mut app.note_base);
        let source_editor = std::mem::take(&mut app.source_editor);
        let edit = std::mem::take(&mut app.edit);
        let note_mode = std::mem::replace(&mut app.note_mode, NoteMode::Read);
        let dirty = std::mem::take(&mut app.dirty);
        let save_error = app.save_error.take();
        let load_error = app.note_load_error.take();
        let code_expanded = std::mem::take(&mut app.note_code_expanded);
        app.blank_note();
        Some(Self {
            text,
            base,
            source_editor,
            edit,
            note_mode,
            dirty,
            save_error,
            load_error,
            code_expanded,
        })
    }

    /// 装回活动字段并重建派生缓存（行高、属性、前后台解析产物、图片表）。
    pub fn install(self, app: &mut App) {
        app.note_text = self.text;
        app.note_base = self.base;
        app.source_editor = self.source_editor;
        app.edit = self.edit;
        app.note_mode = self.note_mode;
        app.dirty = self.dirty;
        app.save_error = self.save_error;
        app.note_load_error = self.load_error;
        app.note_code_expanded = self.code_expanded;
        app.note_scroll_y = 0.0;
        app.reset_note_rows();
        app.reset_note_media();
        app.props.loading = false;
        app.reset_props();
        app.refresh_preview();
        app.refresh_edit();
        app.backlinks.clear();
        app.backlinks_error = None;
        app.history.clear();
        app.space_note_flushed = None;
        if app.dirty {
            app.debounce_tail();
        }
    }
}
