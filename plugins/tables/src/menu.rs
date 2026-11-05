//! 表格右键菜单 / 字段菜单 / 行菜单 / 统计菜单 / 单选菜单：内容由插件声明，宿主渲染并回传选中项。
//! 菜单项 id 自带语义（动作 + 目标 id），选中后在 [`apply`] 里落成模型变更。

use atelyx_format::table::ops::{self, OptState, TableSelection};
use atelyx_plugin_protocol::method::MenuItem;

use crate::input::MenuRequest;
use crate::model::Session;

/// 字段类型档位（字段菜单与新建字段共用同一份；顺序即菜单序）。
pub const FIELD_TYPES: &[(&str, &str)] = &[
    ("text", "文本"),
    ("number", "数字"),
    ("duration", "时长（秒）"),
    ("singleSelect", "单选"),
    ("image", "图片"),
];

/// 按请求构建菜单条目。
pub fn build(session: &Session, request: &MenuRequest) -> Vec<MenuItem> {
    match request {
        MenuRequest::Cell { .. } => vec![
            action("copy", "复制", false),
            action("cut", "剪切", false),
            action("paste", "粘贴", false),
            action("format", "格式", false),
        ],
        MenuRequest::Row { row } => vec![
            action_with(&format!("row:dup:{row}"), "复制行", false),
            action_with(&format!("row:up:{row}"), "上移", false),
            action_with(&format!("row:down:{row}"), "下移", false),
            action_with(&format!("row:auto:{row}"), "行高自适应", false),
            action("format", "格式", false),
            MenuItem::Divider { pane: 0 },
            action_with(&format!("row:del:{row}"), "删除行", true),
        ],
        MenuRequest::Column { field } => vec![
            action_with(&format!("col:auto:{field}"), "列宽自适应", false),
            action("format", "格式", false),
            action_with(&format!("col:insert:{field}:-1"), "左侧插入字段", false),
            action_with(&format!("col:insert:{field}:1"), "右侧插入字段", false),
        ],
        MenuRequest::Field { field } => field_menu(session, field),
        MenuRequest::Status { field } => status_menu(session, field),
        MenuRequest::Select { row, field } => select_menu(session, row, field),
        MenuRequest::StyleFont => font_menu(session),
        MenuRequest::StyleSize => size_menu(session),
        MenuRequest::Lightbox => vec![
            action("lb:copy", "复制图片", false),
            action("lb:download", "下载图片", false),
        ],
        // 整表（左上角）：两个自适应 + 格式，整表 None 语义
        MenuRequest::All => vec![
            action("all:col", "列宽自适应", false),
            action("all:row", "行高自适应", false),
            action("format", "格式", false),
        ],
        // 加列占位：只有「添加字段」一项，点击后开浮层输入
        MenuRequest::AddField => vec![action("field:add", "添加字段", false)],
        // 输入类请求在打开浮层时消化，不进菜单
        MenuRequest::RenameField { .. }
        | MenuRequest::InsertColumn { .. }
        | MenuRequest::EditOptions { .. } => Vec::new(),
    }
}

/// 字段菜单：重命名 + 类型切换（子面板）/ 单选选项 / 删除字段（子面板二次确认）。
fn field_menu(session: &Session, field: &str) -> Vec<MenuItem> {
    let field_type = session
        .data
        .fields
        .iter()
        .find(|item| item.id == field)
        .map(|item| item.field_type.as_str())
        .unwrap_or_default();
    let mut items = vec![
        action(&format!("field:rename:{field}"), "重命名", false),
        MenuItem::Submenu {
            label: "字段类型".to_string(),
            icon: None,
            disabled: false,
            pane: 0,
            target: 2,
        },
    ];
    if field_type == "singleSelect" {
        items.push(action(&format!("field:options:{field}"), "单选选项管理", false));
    }
    items.push(MenuItem::Divider { pane: 0 });
    items.push(MenuItem::Submenu {
        label: "删除字段".to_string(),
        icon: None,
        disabled: false,
        pane: 0,
        target: 3,
    });
    // pane 2：类型列表（当前类型打勾）；pane 3：删除确认
    for (kind, label) in FIELD_TYPES {
        let id = format!("field:retype:{field}:{kind}");
        if field_type == *kind {
            items.push(action_note(&id, label, "当前", 2));
        } else {
            items.push(action_in(&id, label, false, 2));
        }
    }
    items.push(action_in(
        &format!("col:del:{field}"),
        "确认删除（清空该列全部值）",
        false,
        3,
    ));
    items.push(action_in("menu:cancel", "取消", false, 3));
    items
}

/// 状态栏统计菜单：无 + 该字段类型可用的自动计算。
fn status_menu(session: &Session, field: &str) -> Vec<MenuItem> {
    let field_type = session
        .data
        .fields
        .iter()
        .find(|item| item.id == field)
        .map(|item| item.field_type.clone())
        .unwrap_or_default();
    let current = session
        .data
        .fields
        .iter()
        .find(|item| item.id == field)
        .and_then(|item| item.calc_type.clone());
    let mut items = vec![menu_checked(&format!("calc:{field}:"), "无", current.is_none())];
    for kind in ops::calc_types_for(&field_type) {
        let label = ops::calc_type_label(kind);
        items.push(menu_checked(
            &format!("calc:{field}:{kind}"),
            label,
            current.as_deref() == Some(*kind),
        ));
    }
    items
}

/// 单选菜单：未选择 + 选项列表（当前值打勾）。
fn select_menu(session: &Session, row: &str, field: &str) -> Vec<MenuItem> {
    let current = session
        .data
        .rows
        .iter()
        .find(|item| item.id == row)
        .and_then(|item| item.values.get(field))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let options = session
        .data
        .fields
        .iter()
        .find(|item| item.id == field)
        .and_then(|item| item.options.clone())
        .unwrap_or_default();
    let mut items = vec![menu_checked(
        &format!("select:{row}:{field}:"),
        "未选择",
        current.is_none(),
    )];
    for option in options {
        items.push(menu_checked(
            &format!("select:{row}:{field}:{option}"),
            &option,
            current.as_deref() == Some(option.as_str()),
        ));
    }
    items
}

fn action(id: &str, label: &str, danger: bool) -> MenuItem {
    action_with(id, label, danger)
}

fn action_with(id: &str, label: &str, danger: bool) -> MenuItem {
    action_in(id, label, danger, 0)
}

fn action_in(id: &str, label: &str, danger: bool, pane: usize) -> MenuItem {
    MenuItem::Action {
        id: id.to_string(),
        label: label.to_string(),
        icon: None,
        danger,
        disabled: false,
        note: None,
        hint: None,
        pane,
    }
}

/// 带行尾角标的动作项（如当前项标「当前」）。
fn action_note(id: &str, label: &str, note: &str, pane: usize) -> MenuItem {
    MenuItem::Action {
        id: id.to_string(),
        label: label.to_string(),
        icon: None,
        danger: false,
        disabled: false,
        note: Some(note.to_string()),
        hint: None,
        pane,
    }
}

fn menu_checked(id: &str, label: &str, checked: bool) -> MenuItem {
    if checked {
        MenuItem::Checked { id: id.to_string(), label: label.to_string(), pane: 0 }
    } else {
        action(id, label, false)
    }
}

/// 需要浮层输入的菜单项：返回对应请求（其余动作仍走 [`apply`]）。
pub fn prompt_request(id: &str) -> Option<crate::input::MenuRequest> {
    let mut parts = id.split(':');
    let group = parts.next()?;
    let action = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default().to_string();
    match (group, action) {
        ("field", "rename") => Some(crate::input::MenuRequest::RenameField { field_id: target }),
        ("col", "insert") => {
            let side: i32 = parts.next().unwrap_or("1").parse().unwrap_or(1);
            Some(crate::input::MenuRequest::InsertColumn { field_id: target, side })
        }
        ("field", "add") => Some(crate::input::MenuRequest::AddField),
        ("field", "options") => {
            Some(crate::input::MenuRequest::EditOptions { field_id: target })
        }
        _ => None,
    }
}

/// 菜单选中结果 → 模型变更；返回是否有改动（需要置脏 / 排保存）。
pub fn apply(session: &mut Session, id: &str) -> bool {
    let mut parts = id.split(':');
    let group = parts.next().unwrap_or_default();
    match group {
        "copy" | "cut" | "paste" | "clear" => {
            // 剪贴板动作由入口层处理（需要宿主能力）
            false
        }
        // 只切换气泡可见，不改模型（不置脏、不排保存）
        "format" => {
            if session.view.editing.is_none() && session.view.selection.is_some() {
                session.view.bar_shown = true;
            }
            false
        }
        // 整表自适应：与单列 / 单行同一算法，一次改全部
        "all" => {
            let action = parts.next().unwrap_or_default();
            crate::input::push_undo(session);
            let changed = match action {
                "col" => {
                    let widths: Vec<(String, f64)> = session
                        .data
                        .fields
                        .iter()
                        .map(|field| (field.id.clone(), ops::column_auto_width(field, &session.data.rows)))
                        .collect();
                    let mut changed = false;
                    for (field_id, width) in widths {
                        changed |= ops::set_field_width(&mut session.data, &field_id, Some(width));
                    }
                    changed
                }
                "row" => {
                    let rows: Vec<String> =
                        session.data.rows.iter().map(|row| row.id.clone()).collect();
                    ops::clear_row_heights(&mut session.data, &rows)
                }
                _ => false,
            };
            if changed {
                session.touch();
            } else {
                session.view.undo.pop();
            }
            changed
        }
        "row" => {
            let action = parts.next().unwrap_or_default();
            let row_id = parts.next().unwrap_or_default().to_string();
            match action {
                "dup" => {
                    crate::input::push_undo(session);
                    let new_id = nanoid::nanoid!();
                    if ops::duplicate_row(&mut session.data, &row_id, new_id) {
                        session.touch();
                        return true;
                    }
                    session.view.undo.pop();
                    false
                }
                "up" | "down" => {
                    let Some(index) = session.data.rows.iter().position(|row| row.id == row_id)
                    else {
                        return false;
                    };
                    let target = if action == "up" {
                        index.saturating_sub(1)
                    } else {
                        (index + 1).min(session.data.rows.len().saturating_sub(1))
                    };
                    if target == index {
                        return false;
                    }
                    crate::input::push_undo(session);
                    if ops::move_row(&mut session.data, &row_id, target) {
                        session.touch();
                        return true;
                    }
                    session.view.undo.pop();
                    false
                }
                "auto" => {
                    crate::input::push_undo(session);
                    let changed = ops::clear_row_heights(&mut session.data, &[row_id]);
                    if changed {
                        session.touch();
                    } else {
                        session.view.undo.pop();
                    }
                    changed
                }
                "del" => {
                    let Some(index) = session.data.rows.iter().position(|row| row.id == row_id)
                    else {
                        return false;
                    };
                    crate::input::push_undo(session);
                    let removed = ops::remove_rows(&mut session.data, &[row_id]);
                    if removed > 0 {
                        // 选中相邻行便于继续键盘导航：优先原位置的下一行，删的是末行则取上一行
                        let next = if index < session.data.rows.len() {
                            Some(index)
                        } else {
                            index.checked_sub(1)
                        };
                        session.view.selection = next
                            .and_then(|index| session.data.rows.get(index))
                            .map(|row| TableSelection::Row(row.id.clone()));
                        if let Some(index) = next {
                            if !session.data.fields.is_empty() {
                                crate::input::scroll_into_view(session, index, 0);
                            }
                        }
                        crate::geom::clamp_scroll(session);
                        session.touch();
                        true
                    } else {
                        session.view.undo.pop();
                        false
                    }
                }
                _ => false,
            }
        }
        "col" => {
            let action = parts.next().unwrap_or_default();
            let field_id = parts.next().unwrap_or_default().to_string();
            match action {
                "auto" => {
                    let Some(field) = session.data.fields.iter().find(|item| item.id == field_id)
                    else {
                        return false;
                    };
                    let width = ops::column_auto_width(field, &session.data.rows);
                    crate::input::push_undo(session);
                    let changed = ops::set_field_width(&mut session.data, &field_id, Some(width));
                    if changed {
                        session.touch();
                    } else {
                        session.view.undo.pop();
                    }
                    changed
                }
                "insert" => {
                    let side: i32 = parts.next().unwrap_or("1").parse().unwrap_or(1);
                    let Some(index) = session.data.fields.iter().position(|item| item.id == field_id)
                    else {
                        return false;
                    };
                    let at = if side < 0 { index } else { index + 1 };
                    crate::input::push_undo(session);
                    let name = format!("字段{}", session.data.fields.len() + 1);
                    ops::insert_field(&mut session.data, at, nanoid::nanoid!(), &name, "text");
                    session.touch();
                    true
                }
                "del" => {
                    if !session.data.fields.iter().any(|item| item.id == field_id) {
                        return false;
                    }
                    crate::input::push_undo(session);
                    ops::remove_field(&mut session.data, &field_id);
                    crate::input::clear_invalid_selection(session);
                    crate::geom::clamp_scroll(session);
                    session.touch();
                    true
                }
                _ => false,
            }
        }
        "field" => {
            let action = parts.next().unwrap_or_default();
            let field_id = parts.next().unwrap_or_default().to_string();
            match action {
                "rename" => false,
                "retype" => {
                    let kind = parts.next().unwrap_or_default();
                    crate::input::push_undo(session);
                    let changed = ops::retype_field(&mut session.data, &field_id, kind);
                    if changed {
                        session.touch();
                    } else {
                        session.view.undo.pop();
                    }
                    changed
                }
                _ => false,
            }
        }
        "calc" => {
            let field_id = parts.next().unwrap_or_default().to_string();
            let kind = parts.next().unwrap_or_default();
            crate::input::push_undo(session);
            let changed = ops::set_calc_type(
                &mut session.data,
                &field_id,
                (!kind.is_empty()).then_some(kind),
            );
            if changed {
                session.touch();
            } else {
                session.view.undo.pop();
            }
            changed
        }
        "select" => {
            let row_id = parts.next().unwrap_or_default().to_string();
            let field_id = parts.next().unwrap_or_default().to_string();
            let value = parts.collect::<Vec<_>>().join(":");
            crate::input::push_undo(session);
            let value = (!value.is_empty()).then(|| serde_json::Value::String(value));
            let Some(row) = session.data.rows.iter_mut().find(|row| row.id == row_id) else {
                session.view.undo.pop();
                return false;
            };
            let changed = ops::set_cell_value(row, &field_id, value);
            if changed {
                session.touch();
            } else {
                session.view.undo.pop();
            }
            changed
        }
        _ => false,
    }
}


/// 已安装字体族清单（宿主 `query.fonts` 落地；空 = 还没取到，菜单只有「默认」）。
static FONT_FAMILIES: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// 落地字体族清单。
pub fn set_font_families(families: Vec<String>) {
    if let Ok(mut current) = FONT_FAMILIES.write() {
        *current = families;
    }
}

/// 字体菜单：默认 + 已安装族（选区统一为某族才打勾；混合 / 未设均落到「默认」）。
fn font_menu(session: &Session) -> Vec<MenuItem> {
    let (default_checked, current) = match crate::format::selection_style(session).map(|style| style.font)
    {
        Some(OptState::Value(Some(font))) => (false, Some(font)),
        _ => (true, None),
    };
    let mut items = vec![menu_checked("style:font:", "默认", default_checked)];
    if let Ok(families) = FONT_FAMILIES.read() {
        for family in families.iter() {
            items.push(menu_checked(
                &format!("style:font:{family}"),
                family,
                current.as_deref() == Some(family.as_str()),
            ));
        }
    }
    items
}

/// 字号菜单：默认（清除）+ 固定档位。
fn size_menu(_session: &Session) -> Vec<MenuItem> {
    const SIZES: [f64; 7] = [12.0, 13.0, 14.0, 16.0, 18.0, 20.0, 24.0];
    let mut items = vec![action_note("style:size:", "默认", "清除", 0)];
    for size in SIZES {
        items.push(action(&format!("style:size:{}", size as i64), &format!("{}", size as i64), false));
    }
    items
}

/// 样式菜单选中结果：`rest` 形如 `font:<名>` / `size:<号>`。
pub fn apply_style(session: &mut Session, rest: &str) -> bool {
    let (kind, value) = rest.split_once(':').unwrap_or((rest, ""));
    match kind {
        "font" => crate::format::apply(
            session,
            crate::format::patch_font((!value.is_empty()).then(|| value.to_string())),
        ),
        "size" => {
            let size = value.parse::<f64>().ok();
            crate::format::apply(session, crate::format::patch_size(size))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use atelyx_format::table::TableFile;

    use super::TableSelection;
    use crate::model::Session;

    fn table() -> TableFile {
        serde_json::from_value(serde_json::json!({
            "schema": "atelyx-table/v1",
            "id": "t",
            "title": "T",
            "fields": [{ "id": "f", "name": "A", "type": "text" }],
            "rows": [
                { "id": "r0", "values": { "f": "x" } },
                { "id": "r1", "values": { "f": "x" } },
                { "id": "r2", "values": { "f": "x" } }
            ],
        }))
        .expect("表格夹具")
    }

    /// 删行后选中相邻行（优先下一行，末行则上一行），键盘导航不失效。
    #[test]
    fn deleting_row_selects_adjacent_row() {
        let mut session = Session::new("t.atb".into(), table());
        session.view.selection = Some(TableSelection::Row("r1".into()));
        assert!(super::apply(&mut session, "row:del:r1"));
        assert!(matches!(
            session.view.selection,
            Some(TableSelection::Row(ref id)) if id == "r2"
        ));
        assert!(super::apply(&mut session, "row:del:r2"));
        assert!(matches!(
            session.view.selection,
            Some(TableSelection::Row(ref id)) if id == "r0"
        ));
    }
}
