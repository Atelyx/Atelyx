//! 单元格编辑：起稿快照、值解析与提交。编辑状态机在 [`crate::editor::Editor`]，
//! 本模块只负责「单元格 → 值」的语义（数字解析、空值清空、提交有变化才入撤销栈）。

use atelyx_format::table::ops;

use crate::editor::Editor;
use crate::model::{Session, Snapshot};

/// 单元格编辑会话。
pub struct CellEdit {
    pub row: usize,
    pub field: usize,
    pub editor: Editor,
}

/// 起稿：记录快照、把当前值放进编辑器、光标置于末尾。
/// `seed` 给出时以它为初值（选中格敲字符直接覆盖原值）。
pub fn begin(session: &mut Session, row: usize, field: usize, seed: Option<String>) {
    let Some(field_def) = session.data.fields.get(field) else { return };
    let field_id = field_def.id.clone();
    let field_type = field_def.field_type.clone();
    let Some(table_row) = session.data.rows.get(row) else { return };
    let value = table_row.values.get(&field_id);
    let current = match field_type.as_str() {
        "number" | "duration" => value
            .and_then(|value| value.as_f64().or_else(|| value.as_str().and_then(|s| s.parse().ok())))
            .map(|number| format!("{number}"))
            .unwrap_or_default(),
        _ => value
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_default(),
    };
    let seeded = seed.is_some();
    let mut editor = Editor::new(seed.unwrap_or(current));
    editor.touched = seeded;
    session.view.edit_session = Some((session.data.fields.clone(), session.data.rows.clone()));
    session.view.bar_shown = false;
    session.view.editing = Some(CellEdit { row, field, editor });
}

/// 起稿快照入撤销栈（提交有变化时调用）；新变更作废重做栈。
fn push_snapshot(session: &mut Session, snapshot: Snapshot) {
    session.view.undo.push(snapshot);
    if session.view.undo.len() > crate::model::UNDO_DEPTH {
        session.view.undo.remove(0);
    }
    session.view.redo.clear();
}

/// 提交当前编辑：值有变化才入撤销栈并置脏；返回是否有变化。
pub fn commit(session: &mut Session) -> bool {
    let Some(edit) = session.view.editing.take() else { return false };
    let snapshot = session.view.edit_session.take();
    if !edit.editor.touched {
        return false;
    }
    let Some(field) = session.data.fields.get(edit.field) else { return false };
    let field_id = field.id.clone();
    let field_type = field.field_type.clone();
    let value = match cell_value(&field_type, &edit.editor.text) {
        Ok(value) => value,
        // 数字非法：丢弃本次编辑，保留原值；重建布局收回草稿撑开的高度
        Err(()) => {
            session.rebuild_layout();
            crate::geom::clamp_scroll(session);
            return false;
        }
    };
    let Some(row) = session.data.rows.get_mut(edit.row) else { return false };
    let changed = ops::set_cell_value(row, &field_id, value);
    if !changed {
        return false;
    }
    if let Some(snapshot) = snapshot {
        push_snapshot(session, snapshot);
    }
    session.touch();
    true
}

/// 取消编辑（丢弃草稿）。
pub fn cancel(session: &mut Session) -> bool {
    if session.view.editing.take().is_some() {
        session.view.edit_session = None;
    }
    false
}

/// 文本 → 单元格值：文本类存字符串，数字/时长解析为数字。
/// `Err` = 数字非法（调用方丢弃编辑、保留原值）；`Ok(None)` = 输入为空（清值）。
fn cell_value(field_type: &str, text: &str) -> Result<Option<serde_json::Value>, ()> {
    match field_type {
        "number" | "duration" => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            match trimmed.parse::<f64>() {
                Ok(number) if number.is_finite() => Ok(Some(ops::number_value(number))),
                _ => Err(()),
            }
        }
        _ => {
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(serde_json::Value::String(text.to_string())))
            }
        }
    }
}

/// 编辑中的显示文本（含输入法组合串）。
pub fn display_text(edit: &CellEdit) -> String {
    edit.editor.display_text()
}

/// 编辑中的光标与锚点（显示串坐标）。
pub fn display_caret(edit: &CellEdit) -> (usize, usize) {
    edit.editor.display_caret()
}

/// 是否支持多行（文本类字段 Shift+Enter 换行、回车提交）。
pub fn multiline(session: &Session, edit: &CellEdit) -> bool {
    session
        .data
        .fields
        .get(edit.field)
        .is_some_and(|field| field.field_type == "text")
}
