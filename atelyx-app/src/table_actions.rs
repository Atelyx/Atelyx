//! 表格消息处理：交互状态机（按压/拖拽/框选/手势）、编辑会话与撤销、剪贴板 TSV、
//! 图片管线（选图入库 / 句柄缓存 / 灯箱下载）、xlsx 导出与防抖保存链。
//! 模型变更一律经 `table_ops` 纯函数 + `TableSession::touch()`；落盘与磁盘真源
//! 按补丁合并（只写变化实体，外部改动未触及的行/字段不被冲掉）。

use iced::Task;
use iced::{Point, Rectangle, Size};

use crate::table_view as tv;
use crate::{space_actions, App, ImageLoadBatch, Message};
use atelyx_core::image_thumb;
use atelyx_core::table_ops as ops;
use atelyx_core::vault;
use atelyx_core::vault::TableFile;

use iced::widget::text_editor;

use crate::ui::notify::Level as NoticeLevel;

/// 历史合并窗口（毫秒）。
const HISTORY_COALESCE_MS: i64 = 60_000;

pub fn on_msg(app: &mut App, msg: tv::TableMsg) -> Task<Message> {
    if let tv::TableMsg::Menu(inner) = msg {
        // 菜单条目来源：先关弹层再执行。菜单按压非画布按压，同批全局 Press 会按失焦处理——
        // 置 press_seen 抵消（表格保持聚焦），并回置 focused 供 table_active 守卫通过：
        // 异步二段动作（粘贴回执 / 剪切后清空）晚于本批到达，瞬时聚焦不够用
        app.dismiss_popup();
        if let Some(s) = app.table.as_mut() {
            s.view.focused = true;
            s.view.press_seen = true;
        }
        return on_msg_open(app, *inner);
    }
    // 键盘来源的消息在弹层 / 灯箱打开时一律不接（前者自管输入，后者为模态预览）；
    // 表中各分支为键盘唯一入口，画布/菜单/回执来源不受限
    let lightbox_open = app.table.as_ref().is_some_and(|t| t.view.lightbox.is_some());
    if app.popup.is_some() || lightbox_open {
        match msg {
            tv::TableMsg::CharKey(_)
            | tv::TableMsg::NavKey(_)
            | tv::TableMsg::Copy
            | tv::TableMsg::Cut
            | tv::TableMsg::Paste
            | tv::TableMsg::ClearSelection
            | tv::TableMsg::SelectAll
            | tv::TableMsg::Undo
            | tv::TableMsg::Redo => return Task::none(),
            _ => {}
        }
    }
    on_msg_open(app, msg)
}

fn on_msg_open(app: &mut App, msg: tv::TableMsg) -> Task<Message> {
    match msg {
        // Menu 已在 on_msg 入口解包（此处不可达，match 穷尽性要求）
        tv::TableMsg::Menu(_) => Task::none(),
        tv::TableMsg::Loaded(gen, file, result) => on_loaded(app, gen, file, result),
        tv::TableMsg::TableSaved(gen, file, reason, result, frozen) => {
            on_saved(app, gen, file, reason, result, frozen)
        }
        tv::TableMsg::Press { window, pos, size, origin } => on_press(app, window, pos, size, origin),
        tv::TableMsg::Context { window, pos, size } => on_context(app, window, pos, size),
        tv::TableMsg::Move { window, pos, size, origin } => on_move(app, window, pos, size, origin),
        tv::TableMsg::Release(pos) => on_release(app, pos),
        tv::TableMsg::Wheel { delta_y, delta_x } => on_wheel(app, delta_y, delta_x),
        tv::TableMsg::Edited(action) => {
            let Some(s) = app.table.as_mut() else { return Task::none() };
            let Some(edit) = s.view.editing.as_mut() else { return Task::none() };
            // 仅内容编辑动作视为「已输入」（光标移动/选择不算），区分未输入与输入后删空
            if action.is_edit() {
                edit.touched = true;
            }
            // 编辑器内按压必被 text_editor 捕获（外层观察不到），本消息即「按压在编辑器内」
            // 的确定性信号：同批全局 Press 靠 editor_inside/press_seen 放行
            if matches!(
                action,
                text_editor::Action::Click(_)
                    | text_editor::Action::Drag(_)
                    | text_editor::Action::SelectWord
                    | text_editor::Action::SelectLine
            ) {
                s.view.editor_inside = true;
                s.view.press_seen = true;
            }
            edit.content.perform(action);
            Task::none()
        }
        tv::TableMsg::ShowStyleBar => {
            app.dismiss_popup();
            if let Some(s) = app.table.as_mut() {
                if s.view.editing.is_none() && s.view.selection.is_some() {
                    s.view.bar_shown = true;
                }
            }
            Task::none()
        }
        tv::TableMsg::CommitEnter => on_commit_enter(app),
        tv::TableMsg::CancelEdit => {
            cancel_edit(app);
            Task::none()
        }
        tv::TableMsg::CharKey(c) => {
            // 选区态打印字符 = 覆盖输入；仅文本类字段可编辑（image/单选各有专用交互）
            let hit = app.table.as_ref().and_then(|s| {
                if !table_active(s) {
                    return None;
                }
                let (r, ci) = selection_cell(s)?;
                let type_ok = match s.data.fields[ci].field_type.as_str() {
                    "text" => true,
                    "number" | "duration" => c.is_ascii_digit() || matches!(c, '-' | '.' | '+'),
                    _ => false,
                };
                type_ok.then_some((r, ci))
            });
            if let Some((r, ci)) = hit {
                // 初值即用户敲下的字符：会话已属「输入」，提交必须落值
                return begin_edit_with(app, r, ci, c.to_string(), false, true);
            }
            Task::none()
        }
        tv::TableMsg::NavKey(dir) => on_nav_key(app, dir),
        tv::TableMsg::Copy => on_copy(app, false),
        tv::TableMsg::Cut => on_copy(app, true),
        tv::TableMsg::Paste => {
            iced::clipboard::read().map(|t| {
                Message::Table(tv::TableMsg::PasteText(t.unwrap_or_default()))
            })
        }
        tv::TableMsg::PasteText(text) => on_paste(app, text),
        tv::TableMsg::ClearSelection => on_clear_selection(app),
        tv::TableMsg::SelectAll => {
            if let Some(s) = app.table.as_mut() {
                if table_active(s) {
                    s.view.selection = Some(ops::TableSelection::All);
                }
            }
            Task::none()
        }
        tv::TableMsg::ShowBar { row_id, field_id } => {
            if let Some(s) = app.table.as_mut() {
                let selected = matches!(
                    &s.view.selection,
                    Some(ops::TableSelection::Cell { row_id: r, field_id: f }) if *r == row_id && *f == field_id
                );
                if table_active(s) && selected {
                    s.view.bar_shown = true;
                }
            }
            Task::none()
        }
        tv::TableMsg::Undo => {
            undo(app);
            Task::none()
        }
        tv::TableMsg::Redo => {
            redo(app);
            Task::none()
        }
        tv::TableMsg::DuplicateRow(row_id) => {
            with_session(app, |s| {
                push_undo(s);
                if ops::duplicate_row(&mut s.data, &row_id, nanoid::nanoid!()) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::MoveRow { row_id, delta } => {
            with_session(app, |s| {
                let Some(from) = s.data.rows.iter().position(|r| r.id == row_id) else { return };
                let target = if delta < 0 { from.saturating_sub(1) } else { from + 2 };
                push_undo(s);
                if ops::move_row(&mut s.data, &row_id, target) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::DeleteRows(ids) => {
            with_session(app, |s| {
                push_undo(s);
                ops::remove_rows(&mut s.data, &ids);
                s.touch();
                clear_invalid_selection(s);
                clamp_scroll(s);
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::AddField { name, ftype } => {
            with_session(app, |s| {
                push_undo(s);
                let n = if name.trim().is_empty() {
                    format!("字段{}", s.data.fields.len() + 1)
                } else {
                    name.trim().to_string()
                };
                let index = s.data.fields.len();
                ops::insert_field(&mut s.data, index, nanoid::nanoid!(), &n, &ftype);
                s.touch();
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::InsertField { index, name } => {
            with_session(app, |s| {
                push_undo(s);
                let idx = index.unwrap_or(s.data.fields.len());
                let n = if name.trim().is_empty() {
                    format!("字段{}", idx + 1)
                } else {
                    name.trim().to_string()
                };
                ops::insert_field(&mut s.data, idx, nanoid::nanoid!(), &n, "text");
                s.touch();
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::DeleteField(field_id) => {
            with_session(app, |s| {
                push_undo(s);
                ops::remove_field(&mut s.data, &field_id);
                s.touch();
                clear_invalid_selection(s);
                clamp_scroll(s);
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::RenameField { field_id, name } => {
            with_session(app, |s| {
                push_undo(s);
                if ops::rename_field(&mut s.data, &field_id, &name) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::RetypeField { field_id, ftype } => {
            with_session(app, |s| {
                push_undo(s);
                if ops::retype_field(&mut s.data, &field_id, &ftype) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::OptionsAdd { field_id, name } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                return Task::none();
            }
            with_session(app, |s| {
                let mut options = current_options(s, &field_id);
                options.push(name);
                push_undo(s);
                if ops::set_field_options(&mut s.data, &field_id, options) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.table_popup_draft.clear();
            Task::none()
        }
        tv::TableMsg::OptionsRemove { field_id, index } => {
            with_session(app, |s| {
                let mut options = current_options(s, &field_id);
                if index >= options.len() {
                    return;
                }
                options.remove(index);
                push_undo(s);
                if ops::set_field_options(&mut s.data, &field_id, options) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            Task::none()
        }
        tv::TableMsg::ColumnAutoWidth(field_id) => {
            with_session(app, |s| {
                push_undo(s);
                let targets: Vec<String> = match field_id {
                    Some(id) => vec![id],
                    None => s.data.fields.iter().map(|f| f.id.clone()).collect(),
                };
                let mut changed = false;
                for id in targets {
                    let Some(f) = s.data.fields.iter().find(|f| f.id == id).cloned() else { continue };
                    let w = ops::column_auto_width(&f, &s.data.rows);
                    changed |= ops::set_field_width(&mut s.data, &id, Some(w));
                }
                if changed {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::RowHeightAuto(row_id) => {
            with_session(app, |s| {
                push_undo(s);
                let ids: Vec<String> = match row_id {
                    Some(id) => vec![id],
                    None => s.data.rows.iter().filter(|r| r.height.is_some()).map(|r| r.id.clone()).collect(),
                };
                if ops::clear_row_heights(&mut s.data, &ids) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::SetCalc { field_id, calc } => {
            with_session(app, |s| {
                push_undo(s);
                if ops::set_calc_type(&mut s.data, &field_id, calc.as_deref()) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::StyleToggle(key) => apply_style(app, style_toggle_patch(app, key)),
        tv::TableMsg::SetFont(font) => {
            // default = 清除
            let task = apply_style(
                app,
                ops::StylePatch { font: ops::KeyOp::Set(font.filter(|f| f != "default")), ..Default::default() },
            );
            app.dismiss_popup();
            task
        }
        tv::TableMsg::SetSize(size) => {
            let task = apply_style(app, ops::StylePatch { size: ops::KeyOp::Set(size), ..Default::default() });
            app.dismiss_popup();
            task
        }
        tv::TableMsg::SetColor { bg, color } => {
            let patch = if bg {
                ops::StylePatch { bg: ops::KeyOp::Set(color), ..Default::default() }
            } else {
                ops::StylePatch { color: ops::KeyOp::Set(color), ..Default::default() }
            };
            let task = apply_style(app, patch);
            app.dismiss_popup();
            task
        }
        tv::TableMsg::ClearFormat => {
            let task = apply_style(app, clear_format_patch());
            app.dismiss_popup();
            task
        }
        tv::TableMsg::SelectOption { row_id, field_id, value } => {
            with_session(app, |s| {
                push_undo(s);
                let Some(row) = s.data.rows.iter_mut().find(|r| r.id == row_id) else { return };
                let next = value.map(serde_json::Value::String);
                if ops::set_cell_value(row, &field_id, next) {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            app.dismiss_popup();
            Task::none()
        }
        tv::TableMsg::ImageImportPick { row_id, field_id } => pick_images(row_id, field_id),
        tv::TableMsg::ImagesPicked { row_id, field_id, files } => import_images(app, row_id, field_id, files),
        tv::TableMsg::ImagesWritten { row_id, field_id, rels, failed } => {
            if failed > 0 {
                app.notify(NoticeLevel::Warning, format!("有 {failed} 张图片写入失败"));
            }
            if rels.is_empty() {
                return Task::none();
            }
            with_session(app, |s| {
                push_undo(s);
                let Some(row) = s.data.rows.iter_mut().find(|r| r.id == row_id) else {
                    drop_undo(s);
                    return;
                };
                // 旧形态（string[]/标量）不携带 display，一律换新对象；对象形态保留 display
                let mut value = match row.values.get(&field_id) {
                    Some(v) if v.is_object() => v.clone(),
                    _ => serde_json::json!({ "images": [] }),
                };
                let mut images: Vec<serde_json::Value> = value
                    .get("images")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                images.extend(rels.iter().map(|r| serde_json::Value::String(r.clone())));
                value["images"] = serde_json::Value::Array(images);
                row.values.insert(field_id, value);
                s.touch();
            });
            Task::none()
        }
        tv::TableMsg::ImageLoaded { rel, fp, result } => {
            let Some(s) = app.table.as_mut() else { return Task::none() };
            s.view.image_inflight.remove(&rel);
            match result {
                Ok(loaded) => {
                    // 同一路径换了新字节：旧缩略图的常驻登记作废（新句柄另行登记）
                    s.view.pins.borrow_mut().unpin(&rel);
                    s.view.images.insert(rel.clone(), tv::CachedImage::new(fp, loaded));
                    s.view.image_failed.remove(&rel);
                }
                Err(_) => {
                    s.view.image_failed.insert(rel);
                }
            }
            Task::none()
        }
        tv::TableMsg::LightboxClose => {
            if let Some(s) = app.table.as_mut() {
                s.view.lightbox = None;
            }
            Task::none()
        }
        tv::TableMsg::LightboxNav(delta) => {
            let Some(s) = app.table.as_mut() else { return Task::none() };
            let Some(lb) = s.view.lightbox.as_mut() else { return Task::none() };
            let count = s
                .data
                .rows
                .iter()
                .find(|r| r.id == lb.row_id)
                .and_then(|r| r.values.get(&lb.field_id))
                .map(|v| ops::image_entries(v).len())
                .unwrap_or(0);
            if count == 0 {
                return Task::none();
            }
            lb.index = (lb.index as i32 + delta).rem_euclid(count as i32) as usize;
            lb.reset_view();
            Task::none()
        }
        tv::TableMsg::LightboxZoom { pos, view, delta_y } => on_lightbox_zoom(app, pos, view, delta_y),
        tv::TableMsg::LightboxPress(pos) => on_lightbox_press(app, pos),
        tv::TableMsg::LightboxPan { pos, view } => on_lightbox_pan(app, pos, view),
        tv::TableMsg::LightboxDownload => {
            app.dismiss_popup();
            download_lightbox_image(app)
        }
        tv::TableMsg::LightboxCopy => {
            app.dismiss_popup();
            copy_lightbox_image(app)
        }
        tv::TableMsg::Copied(result) => {
            match result {
                Ok(()) => app.notify(NoticeLevel::Success, "已复制图片到剪贴板"),
                Err(e) => app.notify(NoticeLevel::Error, e),
            }
            Task::none()
        }
        tv::TableMsg::OpenLightboxMenu(window) => {
            open_table_popup(app, window, crate::layout_view::PopupState::TableLightboxMenu, None);
            Task::none()
        }
        tv::TableMsg::OpenViewMenu(window) => {
            open_table_popup(app, window, crate::layout_view::PopupState::TableViewMenu, None);
            Task::none()
        }
        tv::TableMsg::SetView(kind) => {
            if let Some(session) = app.table.as_mut() {
                session.view.active_view = kind;
            }
            app.dismiss_popup();
            Task::none()
        }
        // 插件表格写：与本地编辑同一出口（同撤销栈、同置脏与保存链），结果按 request_id 回填
        tv::TableMsg::PluginSetCell { request_id, row_id, field_id, value } => {
            let result = plugin_set_cell(app, &row_id, &field_id, value);
            resolve_plugin_call(app, request_id, result.map(|()| serde_json::json!({})));
            Task::none()
        }
        tv::TableMsg::PluginAddRow { request_id } => {
            let result = plugin_add_row(app);
            resolve_plugin_call(
                app,
                request_id,
                result.map(|row_id| serde_json::json!({ "rowId": row_id })),
            );
            Task::none()
        }
        tv::TableMsg::PluginRemoveRow { request_id, row_id } => {
            let result = plugin_remove_row(app, &row_id);
            resolve_plugin_call(app, request_id, result.map(|()| serde_json::json!({})));
            Task::none()
        }
        tv::TableMsg::PluginSelectRow { request_id, row_id } => {
            let result = plugin_select_row(app, &row_id);
            resolve_plugin_call(app, request_id, result.map(|()| serde_json::json!({})));
            Task::none()
        }
        tv::TableMsg::PluginSetCellImage { request_id, row_id, field_id, images, display } => {
            let result = plugin_set_cell_image(app, &row_id, &field_id, images, display);
            resolve_plugin_call(app, request_id, result.map(|()| serde_json::json!({})));
            Task::none()
        }
        tv::TableMsg::Downloaded(result) => {
            match result {
                Ok(path) => app.notify(NoticeLevel::Success, format!("已保存到 {}", path.display())),
                Err(e) => app.notify(NoticeLevel::Error, e),
            }
            Task::none()
        }
        tv::TableMsg::ExportXlsxPick => {
            app.dismiss_popup();
            // 导出默认名取文件名主名：文件名是唯一真源，会话内嵌标题可能落后于改名
            let title = app
                .table
                .as_ref()
                .map(|s| crate::layout_view::file_label(&s.file))
                .unwrap_or_default();
            Task::perform(
                async move {
                    rfd::AsyncFileDialog::new()
                        .add_filter("Excel 工作簿", &["xlsx"])
                        .set_file_name(format!("{title}.xlsx"))
                        .save_file()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                |path| Message::Table(tv::TableMsg::ExportPathPicked(path)),
            )
        }
        tv::TableMsg::ExportPathPicked(path) => export_xlsx(app, path),
        tv::TableMsg::Exported(result) => {
            match result {
                Ok(0) => app.notify(NoticeLevel::Success, "已导出 xlsx"),
                Ok(n) => app.notify(
                    NoticeLevel::Warning,
                    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
                        format!("已导出 xlsx（{n} 张图片附件在服务端，未嵌入）")
                    } else {
                        format!("已导出 xlsx（{n} 张图片无法嵌入）")
                    },
                ),
                Err(e) => app.notify(NoticeLevel::Error, e),
            }
            Task::none()
        }
        tv::TableMsg::OpenMoreMenu(window) => {
            open_table_popup(app, window, crate::layout_view::PopupState::TableMoreMenu, None);
            Task::none()
        }
        tv::TableMsg::OpenFontMenu(window) => {
            app.font_picker_query.clear();
            open_table_popup(
                app,
                window,
                crate::layout_view::PopupState::FontPicker { for_table: true },
                None,
            );
            Task::none()
        }
        tv::TableMsg::OpenSizeMenu(window) => {
            open_table_popup(app, window, crate::layout_view::PopupState::TableSizeMenu, None);
            Task::none()
        }
        tv::TableMsg::OpenColorMenu { window, bg } => {
            open_table_popup(app, window, crate::layout_view::PopupState::TableColorMenu { bg }, None);
            Task::none()
        }
        tv::TableMsg::PopupDraft(v) => {
            app.table_popup_draft = v;
            Task::none()
        }
        tv::TableMsg::Noop => Task::none(),
    }
}

/// 菜单弹层（锚 = 光标；flip_y 指定则向上翻转）。内联输入草稿随弹层打开清零
/// （弹层内容以空草稿回退显示当前值）。
fn open_table_popup(
    app: &mut App,
    window: iced::window::Id,
    state: crate::layout_view::PopupState,
    flip_y: Option<f32>,
) {
    let point = app.cursors.get(&window).copied().unwrap_or(Point::ORIGIN);
    app.table_popup_draft.clear();
    app.popup = Some((
        window,
        crate::layout_view::PopupOpen { point, flip_y, state, min_width: 0.0 },
    ));
}

/// 菜单弹层（锚 = 给定窗口坐标；单元格锚定的弹层用）。
fn open_table_popup_at(
    app: &mut App,
    window: iced::window::Id,
    state: crate::layout_view::PopupState,
    point: Point,
) {
    app.table_popup_draft.clear();
    app.popup = Some((
        window,
        crate::layout_view::PopupOpen { point, flip_y: None, state, min_width: 0.0 },
    ));
}

// ===== 载入 / 保存 =====

/// 打开 .atb（open_file 按扩展名分流入口）：登记最近表格并载入。
pub fn open_table(app: &mut App, rel: &str) -> Task<Message> {
    app.ui.last_table_file = Some(rel.to_string());
    // 清掉上一表会话：载入在途期间不留旧数据（防串表写盘）
    app.table = None;
    app.history_kind = atelyx_core::history::HistoryKind::Table;
    app.schedule_persist();
    load_table(app, rel)
}

/// 载入 .atb（spawn_blocking + 缓存；回执 gen 守卫）。空间模式走服务端读取。
pub fn load_table(app: &App, rel: &str) -> Task<Message> {
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        return space_actions::space_load_table(app, rel);
    }
    let gen = app.epoch();
    let state = app.vault.clone();
    let rel_owned = rel.to_string();
    Task::perform(
        async move {
            let r = {
                let rel = rel_owned.clone();
                tokio::task::spawn_blocking(move || {
                    let (root, _, _) = state.session_snapshot()?;
                    let (_, mut table) = vault::read_table_file_cached(&state, &root, &rel)?;
                    for row in &mut table.rows {
                        ops::normalize_table_row(row);
                    }
                    Ok::<TableFile, String>(table)
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
            };
            Message::Table(tv::TableMsg::Loaded(gen, rel_owned, r))
        },
        |m| m,
    )
}

fn on_loaded(app: &mut App, gen: u64, file: String, result: Result<TableFile, String>) -> Task<Message> {
    if gen != app.epoch() || app.ui.last_table_file.as_deref() != Some(file.as_str()) {
        return Task::none();
    }
    match result {
        Ok(table) => {
            app.table = Some(tv::TableSession::new(file.clone(), table));
            // 内容载入成功即登记最近打开（本地与空间模式同链回执）
            crate::recent_view::record(app, &file, crate::recent_view::RecentKind::Table);
            // 空间模式在场跟随：名册与协作者据此看到本端正在编辑该表格
            if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
                space_actions::send_presence(app);
            }
            app.reload_history()
        }
        Err(e) => {
            app.notify(NoticeLevel::Error, format!("打开表格失败：{e}"));
            Task::none()
        }
    }
}

/// 应用历史回滚快照到当前表格会话（回滚前状态入撤销栈可撤销、作废 redo、清编辑/选中态）。
/// 会话不存在或文件不匹配返回 false（调用方放弃本次回滚）。
pub fn apply_history_snapshot(app: &mut App, table: TableFile) -> bool {
    let Some(s) = app.table.as_mut() else { return false };
    if s.file != app.ui.last_table_file.clone().unwrap_or_default() {
        return false;
    }
    push_undo(s);
    s.view.redo.clear();
    s.view.editing = None;
    s.view.edit_session = None;
    s.view.selection = None;
    s.view.bar_shown = false;
    s.data = table;
    s.dirty = true;
    s.save_error = None;
    s.version += 1;
    s.rebuild_layout();
    clamp_scroll(s);
    true
}

/// 提交一次表格落盘：与磁盘真源按补丁合并（基线 diff → id 合并 → 队列原子写）。
/// 文件缺失 / 外部替换为其他实体 → 全量重建；写盘成功回 cache。冻结快照随回执
/// 消息带回（不落共享状态），供回执侧判定「落盘期间无新编辑」——并发提交互不覆盖。
pub fn submit_table_save(app: &mut App, reason: tv::SaveReason) -> Task<Message> {
    let Some(s) = app.table.as_ref() else { return Task::none() };
    if !s.dirty {
        return Task::none();
    }
    // 空间模式：基线 diff → 服务端补丁仲裁（id 不符回退全量），回执走 Space 消息
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        return space_actions::space_submit_table_save(app, reason);
    }
    let Some(root) = app.vault.root().ok() else { return Task::none() };
    let gen = app.epoch();
    let file = s.file.clone();
    let baseline = s.saved.clone();
    let frozen = s.data.clone();
    let queue = app.vault.writes.clone();
    let state = app.vault.clone();
    Task::perform(
        async move {
            let r = async {
                let path = vault::safe_join(&root, &file.clone(), false)?;
                let mut disk = match vault::read_table_file_cached(&state, &root, &file.clone()) {
                    // 磁盘被外部替换为其他实体：本端全量重建（等同文件缺失）
                    Ok((_, t)) if t.id == frozen.id => t,
                    _ => frozen.clone(),
                };
                if let Some(patch) = vault::compute_table_patch(&baseline, &frozen) {
                    vault::apply_table_patch(&mut disk, &patch);
                }
                // 内嵌标题随文件名（文件名是唯一真源；内容补丁不改名，此处按路径收取主名）
                disk.title = crate::layout_view::file_label(&file);
                disk.updated_at = now_secs() as i64;
                let json = serde_json::to_string_pretty(&disk).map_err(|e| e.to_string())?;
                queue.write(path.clone(), json).await?;
                vault::cache_put_table(&state, &path, &file, &disk);
                Ok(())
            }
            .await;
            Message::Table(tv::TableMsg::TableSaved(gen, file, reason, r, frozen))
        },
        |m| m,
    )
}

fn on_saved(
    app: &mut App,
    gen: u64,
    file: String,
    reason: tv::SaveReason,
    result: Result<(), String>,
    frozen: TableFile,
) -> Task<Message> {
    // 身份守卫：gen + 文件路径双校验（防串表回执）
    if gen != app.epoch() || !app.table.as_ref().is_some_and(|s| s.file == file) {
        return Task::none();
    }
    match result {
        Err(e) => {
            app.settle_history_restore(false);
            let Some(s) = app.table.as_mut() else { return Task::none() };
            s.save_error = Some(e.clone());
            if matches!(reason, tv::SaveReason::Flush) {
                app.notify(NoticeLevel::Error, format!("保存失败，操作未执行：{e}"));
            } else {
                app.notify(NoticeLevel::Error, format!("保存失败：{e}"));
            }
            Task::none()
        }
        Ok(()) => {
            // 只认磁盘事实：冻结快照与当前数据一致才算落盘；期间又有编辑则保持脏，下一轮补写
            let stable = app
                .table
                .as_ref()
                .is_some_and(|s| s.data.fields == frozen.fields && s.data.rows == frozen.rows);
            let mut task = Task::none();
            {
                let Some(s) = app.table.as_mut() else { return Task::none() };
                s.save_error = None;
                if stable {
                    s.dirty = false;
                    s.saved = frozen.clone();
                }
            }
            if matches!(reason, tv::SaveReason::Flush) {
                task = task.chain(app.after_any_flush());
            }
            if stable {
                app.settle_history_restore(true);
                match serde_json::to_string(&frozen) {
                    Ok(content) => task = task.chain(record_table_history(app, content)),
                    // 序列化失败：宁缺版本也不把空串写入历史（结构体序列化失败为极端异常）
                    Err(e) => app.notify(NoticeLevel::Error, format!("历史版本记录失败：{e}")),
                }
            }
            task
        }
    }
}

fn record_table_history(app: &App, content: String) -> Task<Message> {
    // 空间模式：历史已在保存链内经服务端记录，此处不再本地记版
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        return Task::none();
    }
    let Some(s) = app.table.as_ref() else { return Task::none() };
    let rel = s.file.clone();
    let gen = app.epoch();
    let state = app.vault.clone();
    let author = crate::history_author(app);
    Task::perform(
        async move {
            let r = tokio::task::spawn_blocking(move || {
                let (root, _, _) = state.session_snapshot()?;
                atelyx_core::history::record_version(
                    &root,
                    atelyx_core::history::HistoryKind::Table,
                    &rel,
                    &atelyx_core::history::RecordOpts {
                        content,
                        action: atelyx_core::history::HistoryAction::Edit,
                        author,
                        coalesce_edit_ms: Some(HISTORY_COALESCE_MS),
                        max_versions: 0,
                        note: None,
                    },
                )
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            Message::HistoryRecorded(gen, r.err())
        },
        |m| m,
    )
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ===== 交互 =====

fn with_session(app: &mut App, f: impl FnOnce(&mut tv::TableSession)) {
    if let Some(s) = app.table.as_mut() {
        f(s);
    }
}

/// 回填一次插件表格调用（成功值 / 可读失败原因）。
fn resolve_plugin_call(app: &App, request_id: u64, result: Result<serde_json::Value, String>) {
    app.plugins.resolve_shell_call(request_id, result);
}

/// 插件逐格写：只改指定字段（不整行替换），与本地编辑共用撤销栈与置脏口径。
fn plugin_set_cell(
    app: &mut App,
    row_id: &str,
    field_id: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let Some(session) = app.table.as_mut() else {
        return Err("未打开表格，无法写入".to_string());
    };
    if !session.data.fields.iter().any(|f| f.id == field_id) {
        return Err(format!("字段不存在：{field_id}"));
    }
    if !session.data.rows.iter().any(|r| r.id == row_id) {
        return Err(format!("行不存在：{row_id}"));
    }
    push_undo(session);
    let next = (!value.is_null()).then_some(value);
    let changed = session
        .data
        .rows
        .iter_mut()
        .find(|r| r.id == row_id)
        .is_some_and(|row| ops::set_cell_value(row, field_id, next));
    if changed {
        session.touch();
    } else {
        drop_undo(session);
    }
    Ok(())
}

/// 插件向图片字段单元格写入图片引用：只接受 image 字段，值写成 `{ images, display? }`（走与本地
/// 编辑同一出口：撤销栈、置脏与保存链一致）。
fn plugin_set_cell_image(
    app: &mut App,
    row_id: &str,
    field_id: &str,
    images: Vec<String>,
    display: Option<String>,
) -> Result<(), String> {
    let Some(session) = app.table.as_ref() else {
        return Err("未打开表格，无法写入".to_string());
    };
    let Some(field) = session.data.fields.iter().find(|f| f.id == field_id) else {
        return Err(format!("字段不存在：{field_id}"));
    };
    if field.field_type != "image" {
        return Err(format!("字段不是图片类型：{field_id}"));
    }
    let mut value = serde_json::json!({ "images": images });
    if let Some(display) = display {
        value["display"] = serde_json::Value::String(display);
    }
    plugin_set_cell(app, row_id, field_id, value)
}

/// 插件新增行：返回宿主分配的新行 id（插件据此继续引用该行）。
fn plugin_add_row(app: &mut App) -> Result<String, String> {
    let Some(session) = app.table.as_mut() else {
        return Err("未打开表格，无法新增行".to_string());
    };
    push_undo(session);
    let id = ops::add_row(&mut session.data, nanoid::nanoid!());
    session.touch();
    Ok(id)
}

/// 插件删行：连带清掉失效选中并钳回滚动范围。
fn plugin_remove_row(app: &mut App, row_id: &str) -> Result<(), String> {
    let Some(session) = app.table.as_mut() else {
        return Err("未打开表格，无法删除行".to_string());
    };
    if !session.data.rows.iter().any(|r| r.id == row_id) {
        return Err(format!("行不存在：{row_id}"));
    }
    push_undo(session);
    ops::remove_rows(&mut session.data, &[row_id.to_string()]);
    session.touch();
    clear_invalid_selection(session);
    clamp_scroll(session);
    Ok(())
}

/// 插件选中行：与网格选中态联动；该行不在视口内时把它滚到视口中央。
fn plugin_select_row(app: &mut App, row_id: &str) -> Result<(), String> {
    let Some(session) = app.table.as_mut() else {
        return Err("未打开表格，无法选中行".to_string());
    };
    let Some(index) = session.data.rows.iter().position(|r| r.id == row_id) else {
        return Err(format!("行不存在：{row_id}"));
    };
    session.view.selection = Some(ops::TableSelection::Row(row_id.to_string()));
    let target = scroll_target_for_row(session, index);
    if let Some(next) = target {
        session.view.scroll_y = next;
        clamp_scroll(session);
    }
    Ok(())
}

/// 该行滚到视口中央所需的目标滚动量；行已在视口内或视口尺寸未知 = `None`（不滚动）。
fn scroll_target_for_row(session: &tv::TableSession, index: usize) -> Option<f32> {
    let size = session.view.canvas_size;
    if size.height <= 0.0 {
        return None;
    }
    let layout = session.view.layout.as_ref()?;
    let (y, h) = (layout.row_y.get(index)?.to_owned(), layout.row_h.get(index)?.to_owned());
    let geo = tv::geo(session, size);
    let top = geo.content_y(geo.header_h());
    let bottom = geo.content_y(geo.rows_viewport_bottom());
    (y < top || y + h > bottom).then_some((y - size.height * 0.5 + h * 0.5).max(0.0))
}

/// 内容尺寸缩小时把滚动钳回有效范围（缩放 / 删行删列 / 历史回滚后调用），防整面滚出视口。
fn clamp_scroll(s: &mut tv::TableSession) {
    let (max_x, max_y) = {
        let g = tv::geo(s, s.view.canvas_size);
        (g.max_scroll_x(), g.max_scroll_y())
    };
    s.view.scroll_x = s.view.scroll_x.clamp(0.0, max_x);
    s.view.scroll_y = s.view.scroll_y.clamp(0.0, max_y);
}

fn on_press(app: &mut App, window: iced::window::Id, pos: Point, size: Size, origin: Point) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    s.view.focused = true;
    s.view.press_seen = true;
    s.view.canvas_size = size;
    s.view.canvas_origin = Some((window, origin));
    // 防御：按压落在编辑器矩形内 = 编辑器内部交互（移光标 / 拖选），画布不接管不提交
    // （正常路径下编辑器内按压被 text_editor 捕获、画布收不到；此分支兜底捕获行为变化）
    let in_editor = s
        .view
        .editing
        .as_ref()
        .and_then(|edit| s.cell_index(&edit.row_id, &edit.field_id))
        .is_some_and(|(ri, ci)| {
            let g = tv::geo(s, size);
            let cell = tv::cell_rect(&g, ri, ci);
            let editor = Rectangle::new(
                cell.position(),
                Size::new(cell.width.max(40.0), cell.height.max(24.0)),
            );
            editor.contains(pos)
        });
    if in_editor {
        return Task::none();
    }
    // 先收尾上一格编辑（失焦提交语义），再处理本次按压
    commit_edit(app);
    let Some(s) = app.table.as_mut() else { return Task::none() };
    s.view.bar_shown = false;
    let now = std::time::Instant::now();
    // 双击判定（同格 350ms 内、位移 < 4px）
    let double = s
        .view
        .last_press
        .take()
        .is_some_and(|(t, p)| {
            now.duration_since(t).as_millis() < tv::DOUBLE_CLICK_MS
                && (p.x - pos.x).abs() < 4.0
                && (p.y - pos.y).abs() < 4.0
        });
    s.view.last_press = Some((now, pos));
    let g = tv::geo(s, size);
    let action = match tv::hit_at(&g, pos) {
        tv::Hit::None => {
            s.view.selection = None;
            s.view.editing = None;
            CellAction::None
        }
        tv::Hit::Corner => {
            s.view.selection = Some(ops::TableSelection::All);
            s.view.editing = None;
            CellAction::None
        }
        tv::Hit::AddFieldCol => CellAction::AddFieldMenu,
        tv::Hit::Status(c) => CellAction::StatMenu { field_id: s.data.fields[c].id.clone() },
        tv::Hit::HeaderMenu(c) => CellAction::FieldMenu { field_id: s.data.fields[c].id.clone() },
        tv::Hit::NewRow => {
            push_undo(s);
            ops::add_row(&mut s.data, nanoid::nanoid!());
            s.touch();
            CellAction::None
        }
        tv::Hit::Header(c) => {
            s.view.editing = None;
            s.view.selection = Some(ops::TableSelection::Column(s.data.fields[c].id.clone()));
            CellAction::None
        }
        tv::Hit::HeaderResize(c) => {
            let field = s.data.fields[c].clone();
            let start_w = field.width.unwrap_or_else(|| ops::field_default_width(&field.name));
            s.view.col_drag =
                Some(tv::ColDrag { field_id: field.id, start_x: pos.x, start_w, pushed: false });
            CellAction::None
        }
        tv::Hit::Gutter(r) => {
            s.view.editing = None;
            s.view.selection = Some(ops::TableSelection::Row(s.data.rows[r].id.clone()));
            CellAction::None
        }
        tv::Hit::GutterHandle(r) => {
            let row_id = s.data.rows[r].id.clone();
            let natural = g.cache.row_h[r] / s.view.zoom;
            s.view.row_drag = Some(tv::RowDrag {
                row_id,
                start_y: pos.y,
                start_h: Some(s.data.rows[r].height.unwrap_or(natural as f64)),
                pushed: false,
            });
            CellAction::None
        }
        tv::Hit::GutterGrip(r) => {
            s.view.editing = None;
            s.view.selection = Some(ops::TableSelection::Row(s.data.rows[r].id.clone()));
            s.view.reorder = Some(tv::RowReorder {
                row_id: s.data.rows[r].id.clone(),
                from: r,
                insert_at: r + 1,
                active: false,
                start: pos,
            });
            CellAction::None
        }
        tv::Hit::HScrollbar => {
            let bar = g.hscrollbar();
            let max = g.max_scroll_x();
            let gutter = g.gutter_w();
            // 滑块行程与 hscrollbar() 同口径：行程收在列区视口右缘内（竖条占位时左收）
            let track_w = (g.cols_viewport_right() - gutter).max(1.0);
            if let Some(bar) = bar {
                // 点在滑块外 = 滑块中心跳到点击处；随拖拽按 grab 偏移跟手
                let grab = if bar.contains(pos) {
                    pos.x - bar.x
                } else {
                    let t = ((pos.x - gutter - bar.width / 2.0) / (track_w - bar.width)).clamp(0.0, 1.0);
                    s.view.scroll_x = t * max;
                    bar.width / 2.0
                };
                s.view.hscroll_drag = Some(tv::HScrollDrag { grab });
            }
            CellAction::None
        }
        tv::Hit::VScrollbar => {
            let bar = g.vscrollbar();
            let max = g.max_scroll_y();
            let header = g.header_h();
            // 滑块行程与 vscrollbar() 同口径：轨道高 = 行区视口高（横条占位时上收）
            let track_h = (g.rows_viewport_bottom() - header).max(1.0);
            if let Some(bar) = bar {
                // 点在滑块外 = 滑块中心跳到点击处；随拖拽按 grab 偏移跟手
                let grab = if bar.contains(pos) {
                    pos.y - bar.y
                } else {
                    let t = ((pos.y - header - bar.height / 2.0) / (track_h - bar.height)).clamp(0.0, 1.0);
                    s.view.scroll_y = t * max;
                    bar.height / 2.0
                };
                s.view.vscroll_drag = Some(tv::VScrollDrag { grab });
            }
            CellAction::None
        }
        tv::Hit::Cell(r, c) => cell_press_plan(s, (r, c), pos, double),
    };
    exec_cell_action(app, window, action, origin)
}

/// 单元格按压计划（会话借用内只读判定，动作在借用外交付）。
fn cell_press_plan(
    s: &mut tv::TableSession,
    (r, c): (usize, usize),
    pos: Point,
    double: bool,
) -> CellAction {
    let row_id = s.data.rows[r].id.clone();
    let field_id = s.data.fields[c].id.clone();
    let field_type = s.data.fields[c].field_type.clone();
    if field_type == "image" {
        s.view.editing = None;
        let empty = serde_json::json!({ "images": [] });
        let v = s.data.rows[r].values.get(&field_id).unwrap_or(&empty);
        let cell = tv::cell_rect(&tv::geo(s, s.view.canvas_size), r, c);
        let images = ops::image_entries(v).len();
        // carousel 停留页在图片删减后可能越界，钳到现存末页（绘制侧同口径）
        let idx = s
            .view
            .carousel
            .get(&(row_id.clone(), field_id.clone()))
            .copied()
            .unwrap_or(0)
            .min(images.saturating_sub(1));
        let layout = tv::image_layout_of(cell, v, s.view.zoom, idx);
        let contains = |rect: &Option<Rectangle>| rect.as_ref().is_some_and(|b| b.contains(pos));
        if contains(&layout.mode_btn) {
            return CellAction::ToggleMode { row_id, field_id };
        }
        if contains(&layout.add_btn) || contains(&layout.empty_add) {
            return CellAction::ImportPick { row_id, field_id };
        }
        if contains(&layout.remove_btn) {
            return CellAction::RemoveImage { row_id, field_id, index: idx };
        }
        for (img_idx, _, rect) in layout.thumbs.iter().chain(layout.tiles.iter()) {
            if rect.contains(pos) {
                s.view.carousel.insert((row_id.clone(), field_id.clone()), *img_idx);
                s.view.press = Some(tv::PressState {
                    start: pos,
                    active: false,
                    anchor: (r, c),
                    no_range: true,
                    img: Some(tv::ImagePress {
                        row_id,
                        field_id,
                        kind: tv::ImagePressKind::Thumb(*img_idx),
                        at: std::time::Instant::now(),
                        start: pos,
                    }),
                });
                return CellAction::None;
            }
        }
        if layout.main.is_some_and(|m| m.contains(pos)) && images > 0 {
            s.view.press = Some(tv::PressState {
                start: pos,
                active: false,
                anchor: (r, c),
                no_range: true,
                img: Some(tv::ImagePress {
                    row_id,
                    field_id,
                    kind: tv::ImagePressKind::Main,
                    at: std::time::Instant::now(),
                    start: pos,
                }),
            });
        }
        return CellAction::None;
    }
    if field_type == "singleSelect" {
        s.view.editing = None;
        s.view.selection =
            Some(ops::TableSelection::Cell { row_id: row_id.clone(), field_id: field_id.clone() });
        return CellAction::SelectMenu { row_id, field_id };
    }
    // 文本类：单击 = 选中（再点已选格弹样式工具条）；双击 = 进入编辑（原值起稿）
    let in_selection =
        ops::region_of(s.view.selection.as_ref(), &s.data.fields, &s.data.rows)
            .is_some_and(|region| region.contains(r, c));
    if double {
        s.view.bar_shown = false;
        // 起稿按字段类型取原值：number/duration 的值是 JSON Number（as_str 取不到），格式化为数字字符串
        let field = &s.data.fields[c];
        let v = s.data.rows[r].values.get(&field_id);
        let text = match field.field_type.as_str() {
            "number" | "duration" => v
                .and_then(serde_json::Value::as_f64)
                .map(|n| n.to_string())
                .unwrap_or_default(),
            _ => v.and_then(serde_json::Value::as_str).unwrap_or("").to_string(),
        };
        return CellAction::BeginEdit { r, c, draft: text };
    }
    if in_selection {
        // 延迟弹出样式工具条（双击的第二击落在窗口内则不会弹，避免闪烁）
        s.view.press = Some(tv::PressState {
            start: pos,
            active: false,
            anchor: (r, c),
            no_range: false,
            img: None,
        });
        return CellAction::ShowBar { row_id, field_id };
    }
    s.view.selection = Some(ops::TableSelection::Cell { row_id: row_id.clone(), field_id: field_id.clone() });
    // 框选候选：位移超阈值进入范围选区（image 格已提前返回，不参与框选）
    s.view.press = Some(tv::PressState {
        start: pos,
        active: false,
        anchor: (r, c),
        no_range: false,
        img: None,
    });
    CellAction::None
}

enum CellAction {
    None,
    AddFieldMenu,
    ImportPick { row_id: String, field_id: String },
    SelectMenu { row_id: String, field_id: String },
    BeginEdit { r: usize, c: usize, draft: String },
    ShowBar { row_id: String, field_id: String },
    ToggleMode { row_id: String, field_id: String },
    RemoveImage { row_id: String, field_id: String, index: usize },
    FieldMenu { field_id: String },
    StatMenu { field_id: String },
}

fn exec_cell_action(app: &mut App, window: iced::window::Id, action: CellAction, origin: Point) -> Task<Message> {
    match action {
        CellAction::None => Task::none(),
        CellAction::AddFieldMenu => {
            app.table_popup_draft.clear();
            open_table_popup(app, window, crate::layout_view::PopupState::TableAddField, None);
            Task::none()
        }
        CellAction::ImportPick { row_id, field_id } => {
            Task::perform(async move { (row_id, field_id) }, |(row_id, field_id)| {
                Message::Table(tv::TableMsg::ImageImportPick { row_id, field_id })
            })
        }
        CellAction::SelectMenu { row_id, field_id } => {
            // 弹层锚定单元格左下（窗口坐标 = 画布原点 + 格内偏移；对齐单元格选择弹层位置）
            let anchor = app.table.as_ref().and_then(|s| {
                let (ri, ci) = s.cell_index(&row_id, &field_id)?;
                let g = tv::geo(s, s.view.canvas_size);
                let cell = tv::cell_rect(&g, ri, ci);
                Some(Point::new(origin.x + cell.x, origin.y + cell.y + cell.height + 2.0))
            });
            let state = crate::layout_view::PopupState::TableSelectMenu { row_id, field_id };
            match anchor {
                Some(point) => open_table_popup_at(app, window, state, point),
                None => open_table_popup(app, window, state, None),
            }
            Task::none()
        }
        CellAction::BeginEdit { r, c, draft } => begin_edit_with(app, r, c, draft, true, false),
        CellAction::ShowBar { row_id, field_id } => {
            // 仍选中该格且未进入编辑 = 单击已选格（非双击首击），此时弹样式工具条
            let selected = app.table.as_ref().is_some_and(|s| {
                matches!(&s.view.selection, Some(ops::TableSelection::Cell { row_id: r, field_id: f }) if *r == row_id && *f == field_id)
            });
            if selected {
                Task::perform(
                    async move { tokio::time::sleep(std::time::Duration::from_millis(tv::DOUBLE_CLICK_MS as u64)).await },
                    move |()| Message::Table(tv::TableMsg::ShowBar { row_id, field_id }),
                )
            } else {
                Task::none()
            }
        }
        CellAction::ToggleMode { row_id, field_id } => {
            with_session(app, |s| {
                let Some(is_grid) = s
                    .data
                    .rows
                    .iter()
                    .find(|r| r.id == row_id)
                    .and_then(|r| r.values.get(&field_id))
                    .map(|v| ops::image_display(v) == Some("grid"))
                else {
                    return;
                };
                push_undo(s);
                let Some(row) = s.data.rows.iter_mut().find(|r| r.id == row_id) else {
                    drop_undo(s);
                    return;
                };
                let Some(v) = row.values.get_mut(&field_id) else {
                    drop_undo(s);
                    return;
                };
                let changed = if is_grid {
                    v.as_object_mut().map(|o| o.remove("display")).is_some()
                } else if let Some(obj) = v.as_object_mut() {
                    obj.insert("display".into(), serde_json::Value::String("grid".into()));
                    true
                } else {
                    false
                };
                if changed {
                    s.touch();
                } else {
                    drop_undo(s);
                }
            });
            Task::none()
        }
        CellAction::RemoveImage { row_id, field_id, index } => {
            with_session(app, |s| {
                push_undo(s);
                let Some(images) = row_images_of(s, &row_id, &field_id) else {
                    drop_undo(s);
                    return;
                };
                if index >= images.len() {
                    drop_undo(s);
                    return;
                }
                let mut images = images;
                images.remove(index);
                write_row_images(s, &row_id, &field_id, images);
                s.touch();
            });
            Task::none()
        }
        CellAction::FieldMenu { field_id } => {
            open_table_popup(
                app,
                window,
                crate::layout_view::PopupState::TableFieldMenu { field_id },
                None,
            );
            Task::none()
        }
        CellAction::StatMenu { field_id } => {
            open_table_popup(
                app,
                window,
                crate::layout_view::PopupState::TableStatMenu { field_id },
                Some(app.cursors.get(&window).copied().unwrap_or(Point::ORIGIN).y),
            );
            Task::none()
        }
    }
}

fn on_context(app: &mut App, window: iced::window::Id, pos: Point, size: Size) -> Task<Message> {
    let point = app.cursors.get(&window).copied().unwrap_or(Point::ORIGIN);
    commit_edit(app);
    let Some(s) = app.table.as_mut() else { return Task::none() };
    s.view.focused = true;
    s.view.canvas_size = size;
    s.view.bar_shown = false;
    let g = tv::geo(s, size);
    // 右键点在当前选区内保留选区，否则落单格 / 整行 / 整列
    let in_region = |r: usize, c: usize| {
        ops::region_of(s.view.selection.as_ref(), &s.data.fields, &s.data.rows)
            .is_some_and(|region| region.contains(r, c))
    };
    let state = match tv::hit_at(&g, pos) {
        tv::Hit::Corner => {
            s.view.selection = Some(ops::TableSelection::All);
            Some(crate::layout_view::PopupState::TableSelectAllMenu)
        }
        tv::Hit::Header(c) => {
            s.view.editing = None;
            s.view.selection = Some(ops::TableSelection::Column(s.data.fields[c].id.clone()));
            Some(crate::layout_view::PopupState::TableColumnMenu { field_id: s.data.fields[c].id.clone() })
        }
        tv::Hit::HeaderMenu(c) => {
            Some(crate::layout_view::PopupState::TableFieldMenu { field_id: s.data.fields[c].id.clone() })
        }
        tv::Hit::Gutter(r) => {
            s.view.editing = None;
            s.view.selection = Some(ops::TableSelection::Row(s.data.rows[r].id.clone()));
            Some(crate::layout_view::PopupState::TableRowMenu { row_id: s.data.rows[r].id.clone() })
        }
        tv::Hit::Cell(r, c) => {
            if !in_region(r, c) {
                s.view.selection = Some(ops::TableSelection::Cell {
                    row_id: s.data.rows[r].id.clone(),
                    field_id: s.data.fields[c].id.clone(),
                });
            }
            Some(crate::layout_view::PopupState::TableCellMenu)
        }
        tv::Hit::Status(c) => {
            Some(crate::layout_view::PopupState::TableStatMenu { field_id: s.data.fields[c].id.clone() })
        }
        _ => None,
    };
    if let Some(state) = state {
        // 状态栏计算菜单向上弹出
        let flip = matches!(state, crate::layout_view::PopupState::TableStatMenu { .. }).then_some(point.y);
        app.popup = Some((
            window,
            crate::layout_view::PopupOpen { point, flip_y: flip, state, min_width: 0.0 },
        ));
    }
    Task::none()
}

fn on_move(app: &mut App, window: iced::window::Id, pos: Point, size: Size, origin: Point) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    s.view.hover = pos;
    // 每次画布悬停都回写原点：格式气泡的窗口锚点须与本窗口当前画布位置同源
    s.view.canvas_origin = Some((window, origin));
    if s.view.canvas_size != size {
        // 首帧悬停播种画布尺寸（打开表格后图片即按可见区加载，无需先按压）；
        // 尺寸变化收口滚动视口，旧滚动值一并钳回新上限
        s.view.canvas_size = size;
        clamp_scroll(s);
    }
    // 竖向滚动条拖拽（滑块按 grab 偏移跟手）
    let vscroll = s.view.vscroll_drag.as_ref().map(|d| d.grab).map(|grab| {
        let g = tv::geo(s, size);
        (grab, g.vscrollbar(), g.max_scroll_y(), g.header_h(), g.rows_viewport_bottom())
    });
    if let Some((grab, bar, max, header, viewport_bottom)) = vscroll {
        if let Some(bar) = bar {
            let track_h = (viewport_bottom - header).max(1.0);
            let t = ((pos.y - header - grab) / (track_h - bar.height)).clamp(0.0, 1.0);
            s.view.scroll_y = t * max;
        }
        return Task::none();
    }
    // 横向滚动条拖拽（滑块按 grab 偏移跟手）
    let hscroll = s.view.hscroll_drag.as_ref().map(|d| d.grab).map(|grab| {
        let g = tv::geo(s, size);
        (grab, g.hscrollbar(), g.max_scroll_x(), g.gutter_w(), g.cols_viewport_right())
    });
    if let Some((grab, bar, max, gutter, viewport_right)) = hscroll {
        if let Some(bar) = bar {
            let track_w = (viewport_right - gutter).max(1.0);
            let t = ((pos.x - gutter - grab) / (track_w - bar.width)).clamp(0.0, 1.0);
            s.view.scroll_x = t * max;
        }
        return Task::none();
    }
    // 先取会话快照（字段级读取），再做需要 &mut / &geo 的动作——避免借用交叠
    let col_drag = s
        .view
        .col_drag
        .as_ref()
        .map(|d| (d.field_id.clone(), d.start_x, d.start_w, d.pushed));
    if let Some((field_id, start_x, start_w, pushed)) = col_drag {
        let w = (start_w + ((pos.x - start_x) / s.view.zoom) as f64)
            .clamp(ops::MIN_COL_WIDTH, ops::MAX_COL_WIDTH);
        let changed = w != start_w;
        if !pushed && changed {
            push_undo(s);
            s.view.col_drag.as_mut().expect("col_drag 已判存在").pushed = true;
        }
        if (pushed || changed) && ops::set_field_width(&mut s.data, &field_id, Some(w)) {
            s.touch();
        }
        return Task::none();
    }
    let row_drag = s
        .view
        .row_drag
        .as_ref()
        .map(|d| (d.row_id.clone(), d.start_y, d.start_h, d.pushed));
    if let Some((row_id, start_y, start_h, pushed)) = row_drag {
        let h = (start_h.unwrap_or(32.0) + ((pos.y - start_y) / s.view.zoom) as f64)
            .clamp(ops::MIN_ROW_HEIGHT, ops::MAX_ROW_HEIGHT);
        let changed = Some(h) != start_h;
        if !pushed && changed {
            push_undo(s);
            s.view.row_drag.as_mut().expect("row_drag 已判存在").pushed = true;
        }
        if (pushed || changed) && ops::set_row_height(&mut s.data, &row_id, Some(h)) {
            s.touch();
        }
        return Task::none();
    }
    let reorder = s.view.reorder.as_ref().map(|r| (r.active, r.start));
    if let Some((active, start)) = reorder {
        if !active && distance(pos, start) > tv::DRAG_THRESHOLD {
            s.view.reorder.as_mut().expect("reorder 已判存在").active = true;
        }
        let active = s.view.reorder.as_ref().is_some_and(|r| r.active);
        if active {
            let g = tv::geo(s, size);
            let cy = g.content_y(pos.y);
            let mut at = s.data.rows.len();
            for (i, y) in g.cache.row_y.iter().enumerate() {
                if cy < y + g.cache.row_h[i] / 2.0 {
                    at = i;
                    break;
                }
            }
            s.view.reorder.as_mut().expect("reorder 已判存在").insert_at = at;
        }
        return Task::none();
    }
    if let Some(drag) = s.view.img_drag.as_mut() {
        drag.pos = pos;
        return Task::none();
    }
    if let Some(sw) = s.view.swipe.as_mut() {
        let dx = pos.x - sw.start_x;
        let edge = (sw.index == 0 && dx > 0.0) || (sw.index + 1 >= sw.count && dx < 0.0);
        sw.dx = if edge { dx * tv::IMAGE_EDGE_DAMPING } else { dx };
        return Task::none();
    }
    // 框选（image 字段不参与）：命中换算先做（不可变借用），再落选区
    let press_img = s
        .view
        .press
        .as_ref()
        .and_then(|p| p.img.as_ref().map(|i| (i.row_id.clone(), i.field_id.clone(), i.kind, i.at, i.start)));
    if let Some((row_id, field_id, kind, _at, start)) = press_img {
        let moved = distance(pos, start);
        let was_active = s.view.press.as_ref().is_some_and(|p| p.active);
        if !was_active && moved > tv::IMAGE_PRESS_CANCEL {
            let press = s.view.press.as_mut().expect("press 已判存在");
            press.active = true;
            if let tv::ImagePressKind::Main = kind {
                let idx = s
                    .view
                    .carousel
                    .get(&(row_id.clone(), field_id.clone()))
                    .copied()
                    .unwrap_or(0);
                let count = row_images_len(s, &row_id, &field_id);
                s.view.swipe = Some(tv::SwipeState {
                    row_id,
                    field_id,
                    index: idx,
                    count,
                    start_x: start.x,
                    dx: 0.0,
                });
            }
        }
        return Task::none();
    }
    let press_state = s.view.press.as_ref().map(|p| (p.start, p.active, p.anchor, p.no_range));
    let Some((start, mut active, anchor, no_range)) = press_state else { return Task::none() };
    if !active && distance(pos, start) > tv::DRAG_THRESHOLD {
        active = true;
        s.view.press.as_mut().expect("press 已判存在").active = true;
    }
    if active && !no_range {
        let g = tv::geo(s, size);
        let (Some(r), Some(c)) = (g.row_at(g.content_y(pos.y)), g.col_at(g.content_x(pos.x))) else {
            return Task::none();
        };
        let (ar, ac) = anchor;
        if r < s.data.rows.len()
            && c < s.data.fields.len()
            && ar < s.data.rows.len()
            && ac < s.data.fields.len()
        {
            s.view.selection = Some(ops::TableSelection::Range {
                anchor_row_id: s.data.rows[ar].id.clone(),
                anchor_field_id: s.data.fields[ac].id.clone(),
                row_id: s.data.rows[r].id.clone(),
                field_id: s.data.fields[c].id.clone(),
            });
        }
    }
    Task::none()
}

fn on_release(app: &mut App, pos: Point) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    s.view.hover = pos;
    if s.view.vscroll_drag.take().is_some() || s.view.hscroll_drag.take().is_some() {
        return Task::none();
    }
    if s.view.col_drag.take().is_some() || s.view.row_drag.take().is_some() {
        // 拖拽期间被拖列不实测（见 `rebuild_layout`）：松手后补一次重建，行高与行段回到实测口径
        s.rebuild_layout();
        clamp_scroll(s);
        return Task::none();
    }
    if let Some(reo) = s.view.reorder.take() {
        if reo.active {
            push_undo(s);
            if ops::move_row(&mut s.data, &reo.row_id, reo.insert_at) {
                s.touch();
            } else {
                drop_undo(s);
            }
        }
        return Task::none();
    }
    if let Some(drag) = s.view.img_drag.take() {
        commit_image_drag(s, drag);
        return Task::none();
    }
    if let Some(sw) = s.view.swipe.take() {
        // 翻页判定：|dx| ≥ 阈值，否则回弹（dx 归零即回弹）
        if sw.dx.abs() >= tv::IMAGE_SWIPE_THRESHOLD * s.view.zoom {
            let dir = if sw.dx < 0.0 { 1i32 } else { -1 };
            let next = (sw.index as i32 + dir).clamp(0, sw.count as i32 - 1) as usize;
            s.view.carousel.insert((sw.row_id, sw.field_id), next);
        }
        return Task::none();
    }
    let Some(press) = s.view.press.take() else { return Task::none() };
    if press.active {
        return Task::none();
    }
    // 干净松手：主图/九宫格 = 灯箱；轮播缩略图已在按下时跳转
    let Some(img) = press.img else { return Task::none() };
    let idx = s
        .view
        .carousel
        .get(&(img.row_id.clone(), img.field_id.clone()))
        .copied()
        .unwrap_or(0);
    match img.kind {
        tv::ImagePressKind::Main => {
            s.view.lightbox = Some(tv::Lightbox::new(img.row_id, img.field_id, idx));
        }
        tv::ImagePressKind::Thumb(i) => {
            let is_grid = s
                .data
                .rows
                .iter()
                .find(|r| r.id == img.row_id)
                .and_then(|r| r.values.get(&img.field_id))
                .and_then(|v| ops::image_display(v))
                == Some("grid");
            if is_grid {
                s.view.lightbox = Some(tv::Lightbox::new(img.row_id, img.field_id, i));
            }
        }
    }
    Task::none()
}

fn on_wheel(app: &mut App, delta_y: f32, delta_x: f32) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    if app.mods.control() {
        // Ctrl+滚轮缩放（winit 滚上为正 → 放大）：exp 曲线 + 两位小数
        let z = (s.view.zoom * (delta_y * 0.0015f32).exp()).clamp(tv::MIN_ZOOM, tv::MAX_ZOOM);
        s.view.zoom = (z * 100.0).round() / 100.0;
        s.rebuild_layout();
        clamp_scroll(s);
        return Task::none();
    }
    let (max_x, max_y) = {
        let g = tv::geo(s, s.view.canvas_size);
        (g.max_scroll_x(), g.max_scroll_y())
    };
    // winit 滚上 / 滚右为正：视口取负（内容随手势同向移动）
    if app.mods.shift() || app.mods.alt() {
        // Shift / Alt 横滚：普通鼠标只有纵向分量，退回 delta_y 驱动左右
        let d = if delta_x != 0.0 { delta_x } else { delta_y };
        s.view.scroll_x = (s.view.scroll_x - d).clamp(0.0, max_x);
    } else {
        s.view.scroll_y = (s.view.scroll_y - delta_y).clamp(0.0, max_y);
        s.view.scroll_x = (s.view.scroll_x - delta_x).clamp(0.0, max_x);
    }
    Task::none()
}

fn distance(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

// ===== 灯箱缩放 / 平移 =====

/// 滚轮缩放：以光标为锚点，倍率区间 [适应窗口, 上限]。
fn on_lightbox_zoom(app: &mut App, pos: Point, view: Size, delta_y: f32) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    let Some((w, h)) = s
        .view
        .lightbox
        .as_ref()
        .and_then(|lb| tv::lightbox_image(s, lb))
        .map(|c| c.size)
        .filter(|(w, h)| *w > 0.0 && *h > 0.0)
    else {
        return Task::none();
    };
    let Some(lb) = s.view.lightbox.as_mut() else { return Task::none() };
    let next = tv::lightbox_zoom_level(lb.zoom, delta_y);
    if (next - lb.zoom).abs() < f32::EPSILON {
        return Task::none();
    }
    lb.pan = tv::lightbox_zoom_pan(view, (w, h), lb.zoom, next, lb.pan, pos);
    lb.zoom = next;
    Task::none()
}

/// 图片按压：双击复位；放大后按住即进入拖动。
fn on_lightbox_press(app: &mut App, pos: Point) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    let Some(lb) = s.view.lightbox.as_mut() else { return Task::none() };
    let now = std::time::Instant::now();
    let double = lb.last_press.take().is_some_and(|(t, p)| {
        now.duration_since(t).as_millis() < tv::DOUBLE_CLICK_MS
            && (p.x - pos.x).abs() < tv::LIGHTBOX_CLICK_SLOP
            && (p.y - pos.y).abs() < tv::LIGHTBOX_CLICK_SLOP
    });
    if double {
        lb.reset_view();
    } else {
        lb.last_press = Some((now, pos));
        lb.drag = Some(pos);
    }
    Task::none()
}

/// 拖动平移：按光标位移更新偏移（None = 松开）。
fn on_lightbox_pan(app: &mut App, pos: Option<Point>, view: Size) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    let img = s
        .view
        .lightbox
        .as_ref()
        .and_then(|lb| tv::lightbox_image(s, lb))
        .map(|c| c.size)
        .filter(|(w, h)| *w > 0.0 && *h > 0.0);
    let Some(lb) = s.view.lightbox.as_mut() else { return Task::none() };
    match pos {
        Some(p) => {
            if let Some(last) = lb.drag {
                if let Some(img) = img {
                    lb.pan = tv::lightbox_clamp_pan(view, img, lb.zoom, lb.pan + (p - last));
                }
                lb.drag = Some(p);
            }
        }
        None => lb.drag = None,
    }
    Task::none()
}

// ===== 编辑会话 =====

fn selection_cell(s: &tv::TableSession) -> Option<(usize, usize)> {
    match s.view.selection.as_ref()? {
        ops::TableSelection::Cell { row_id, field_id } => s.cell_index(row_id, field_id),
        _ => None,
    }
}

/// 进入单元格编辑（双击 / 首字符；即编辑会话入口）。
/// `from_press` = 按压发起（双击）：同批全局 Press 会走「失焦提交」分支，置位编辑器内标志让其放行；
/// 键盘发起同批无 Press，不置位（否则标志残留吞掉下一次画布外点击的提交）。
/// `touched` = 起稿文本本身即用户输入（选中后敲字，初值就是敲下的字符）：置位后提交即落值；
/// 双击取原值起稿不置位（未输入 = 放弃会话，原值保留）。
fn begin_edit_with(
    app: &mut App,
    r: usize,
    c: usize,
    draft: String,
    from_press: bool,
    touched: bool,
) -> Task<Message> {
    let Some(s) = app.table.as_mut() else { return Task::none() };
    let (row_id, field_id) = (s.data.rows[r].id.clone(), s.data.fields[c].id.clone());
    s.view.edit_session = Some(tv::EditSession {
        row_id: row_id.clone(),
        field_id: field_id.clone(),
        snapshot: (s.data.fields.clone(), s.data.rows.clone()),
    });
    s.view.bar_shown = false;
    s.view.editor_inside = from_press;
    // 双击首击的框选候选随编辑开始作废（编辑中移动鼠标不得改选区）
    s.view.press = None;
    let mut content = if draft.is_empty() {
        text_editor::Content::new()
    } else {
        text_editor::Content::with_text(&draft)
    };
    // 起稿光标落内容尾部（Move 非编辑动作，不计 touched）
    content.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    s.view.editing = Some(tv::CellEdit { row_id, field_id, content, touched });
    iced::widget::operation::focus(iced::widget::Id::new(tv::CELL_EDITOR_ID))
        .map(|()| Message::Table(tv::TableMsg::Noop))
}

/// 提交当前编辑（Enter / 失焦 / 打开关闭切仓库前的收尾）。
/// 提交语义：未输入 = 放弃会话（原值保留）；输入后删空 = 清值；有变化 = 起稿快照入撤销栈后落值；
/// 无变化 / 数字非法 = 丢弃会话，不动数据与撤销栈。
pub fn commit_edit(app: &mut App) {
    let Some(s) = app.table.as_mut() else { return };
    let Some(edit) = s.view.editing.take() else { return };
    let Some(session) = s.view.edit_session.take() else { return };
    if !edit.touched {
        return;
    }
    let text = edit.content.text();
    let Some(field) = s.data.fields.iter().find(|f| f.id == edit.field_id) else { return };
    let next = match field.field_type.as_str() {
        "number" | "duration" => {
            let t = text.trim();
            if t.is_empty() {
                None
            } else {
                match t.parse::<f64>() {
                    // 非法输入 = 放弃本次编辑（数据未动，丢弃会话即可）
                    Ok(n) if n.is_finite() => Some(ops::number_value(n)),
                    _ => return,
                }
            }
        }
        _ => (!text.is_empty()).then_some(serde_json::Value::String(text)),
    };
    // 变化判定与 set_cell_value 同口径（空串 ≈ 缺键等归一化）：否则压入无差异的幽灵撤销步
    let changed = s
        .data
        .rows
        .iter()
        .find(|r| r.id == edit.row_id)
        .is_some_and(|row| !ops::cell_value_equal(row.values.get(&edit.field_id), next.as_ref()));
    if !changed {
        return;
    }
    push_snapshot(s, session.snapshot);
    if let Some(row) = s.data.rows.iter_mut().find(|r| r.id == edit.row_id) {
        if ops::set_cell_value(row, &edit.field_id, next) {
            s.touch();
        }
    }
}

/// Enter 提交并下移一行（保持同列选中）。
fn on_commit_enter(app: &mut App) -> Task<Message> {
    let plan = app.table.as_ref().and_then(|s| {
        let edit = s.view.editing.as_ref()?;
        let (r, c) = s.cell_index(&edit.row_id, &edit.field_id)?;
        Some((r, c, r + 1 < s.data.rows.len()))
    });
    let Some((r, c, has_next)) = plan else { return Task::none() };
    commit_edit(app);
    if !has_next {
        return Task::none();
    }
    let Some(s) = app.table.as_mut() else { return Task::none() };
    s.view.selection = Some(ops::TableSelection::Cell {
        row_id: s.data.rows[r + 1].id.clone(),
        field_id: s.data.fields[c].id.clone(),
    });
    Task::none()
}

/// Esc：取消编辑（草稿仅在编辑器中，数据未动；直接丢弃会话）；清编辑与选区。
fn cancel_edit(app: &mut App) {
    let Some(s) = app.table.as_mut() else { return };
    s.view.edit_session = None;
    s.view.editing = None;
    s.view.selection = None;
}

// ===== 撤销 =====

// 快照式撤销，栈深 UNDO_DEPTH；选中 ≠ 会话——导航/单击不产生撤销单元。

/// 压入快照并维持栈深上限（会话快照在提交变化时、常规操作在变更前调用）。
fn push_snapshot(s: &mut tv::TableSession, snap: tv::TableSnapshot) {
    s.view.undo.push(snap);
    if s.view.undo.len() > tv::UNDO_DEPTH {
        s.view.undo.remove(0);
    }
}

fn push_undo(s: &mut tv::TableSession) {
    let snap = (s.data.fields.clone(), s.data.rows.clone());
    push_snapshot(s, snap);
}

/// 回退刚压入的快照（无操作时丢弃，不入撤销栈）。
fn drop_undo(s: &mut tv::TableSession) {
    s.view.undo.pop();
}

fn undo(app: &mut App) {
    // 与 redo 同规：编辑会话中撤销会与编辑器草稿错位（提交/取消先于撤销）
    if !app
        .table
        .as_ref()
        .is_some_and(|s| s.view.focused && s.view.editing.is_none())
    {
        return;
    }
    let Some(s) = app.table.as_mut() else { return };
    let Some(snap) = s.view.undo.pop() else { return };
    s.view.redo.push((s.data.fields.clone(), s.data.rows.clone()));
    ops::restore(&mut s.data, snap);
    s.dirty = true;
    s.save_error = None;
    s.version += 1;
    s.rebuild_layout();
    s.view.bar_shown = false;
    clear_invalid_selection(s);
}

fn redo(app: &mut App) {
    // 编辑会话中重做会与编辑器草稿错位（编辑态撤销后重做交由重新进入编辑承担）
    if !app.table.as_ref().is_some_and(|s| s.view.focused && s.view.editing.is_none()) {
        return;
    }
    let Some(s) = app.table.as_mut() else { return };
    let Some(snap) = s.view.redo.pop() else { return };
    push_undo(s);
    ops::restore(&mut s.data, snap);
    s.dirty = true;
    s.save_error = None;
    s.version += 1;
    s.rebuild_layout();
    s.view.bar_shown = false;
    clear_invalid_selection(s);
}

/// 行/字段增删后清失效选区（任一端 id 消失）。
fn clear_invalid_selection(s: &mut tv::TableSession) {
    let row_ok = |id: &str| s.data.rows.iter().any(|r| r.id == id);
    let field_ok = |id: &str| s.data.fields.iter().any(|f| f.id == id);
    let ok = match s.view.selection.clone() {
        Some(ops::TableSelection::Cell { row_id, field_id }) => row_ok(&row_id) && field_ok(&field_id),
        Some(ops::TableSelection::Range { anchor_row_id, anchor_field_id, row_id, field_id }) => {
            row_ok(&anchor_row_id) && field_ok(&anchor_field_id) && row_ok(&row_id) && field_ok(&field_id)
        }
        Some(ops::TableSelection::Row(id)) => row_ok(&id),
        Some(ops::TableSelection::Column(id)) => field_ok(&id),
        Some(ops::TableSelection::All) => true,
        None => true,
    };
    if !ok {
        s.view.selection = None;
    }
}

// ===== 剪贴板 =====

/// 键盘/菜单路径的动作门槛：画布持有焦点且不在单元格编辑态
/// （编辑态的剪贴板/撤销由编辑器部件与全局分派各自承担，不落选区路径）。
fn table_active(s: &tv::TableSession) -> bool {
    s.view.focused && s.view.editing.is_none()
}

fn on_copy(app: &mut App, cut: bool) -> Task<Message> {
    let Some(s) = app.table.as_ref().filter(|s| table_active(s)) else { return Task::none() };
    let Some(region) = ops::region_of(s.view.selection.as_ref(), &s.data.fields, &s.data.rows) else {
        return Task::none();
    };
    let tsv = ops::build_region_tsv(&s.data.fields, &s.data.rows, &region);
    let write = iced::clipboard::write(tsv);
    if cut {
        write.chain(Task::perform(async {}, |_| Message::Table(tv::TableMsg::ClearSelection)))
    } else {
        write
    }
}

fn on_paste(app: &mut App, text: String) -> Task<Message> {
    let grid = ops::parse_tsv(&text);
    if grid.is_empty() {
        return Task::none();
    }
    with_session(app, |s| {
        if !table_active(s) {
            return;
        }
        let Some(region) = ops::region_of(s.view.selection.as_ref(), &s.data.fields, &s.data.rows) else {
            return;
        };
        push_undo(s);
        if ops::apply_paste_grid(
            &mut s.data.fields,
            &mut s.data.rows,
            region.row_start,
            region.col_start,
            &grid,
            || nanoid::nanoid!(),
        ) {
            s.touch();
        } else {
            drop_undo(s);
        }
    });
    Task::none()
}

fn on_clear_selection(app: &mut App) -> Task<Message> {
    with_session(app, |s| {
        if !table_active(s) {
            return;
        }
        let Some(region) = ops::region_of(s.view.selection.as_ref(), &s.data.fields, &s.data.rows) else {
            return;
        };
        push_undo(s);
        let mut changed = false;
        for r in region.row_start..=region.row_end {
            for c in region.col_start..=region.col_end {
                let field_id = s.data.fields[c].id.clone();
                if s.data.fields[c].field_type == "image" {
                    continue;
                }
                changed |= ops::set_cell_value(&mut s.data.rows[r], &field_id, None);
            }
        }
        if changed {
            s.touch();
        } else {
            drop_undo(s);
        }
    });
    Task::none()
}

fn on_nav_key(app: &mut App, dir: tv::NavDir) -> Task<Message> {
    let Some(s) = app.table.as_ref().filter(|s| table_active(s)) else { return Task::none() };
    let (r, c) = match selection_cell(s) {
        Some(rc) => rc,
        None => return Task::none(),
    };
    let last_r = s.data.rows.len().saturating_sub(1);
    let last_c = s.data.fields.len().saturating_sub(1);
    let (nr, nc) = match dir {
        tv::NavDir::Up => (r.saturating_sub(1), c),
        tv::NavDir::Down => ((r + 1).min(last_r), c),
        tv::NavDir::Left => (r, c.saturating_sub(1)),
        tv::NavDir::Right => (r, (c + 1).min(last_c)),
        tv::NavDir::Home => (r, 0),
        tv::NavDir::End => (r, last_c),
    };
    if nr == r && nc == c {
        return Task::none();
    }
    let Some(s) = app.table.as_mut() else { return Task::none() };
    s.view.selection = Some(ops::TableSelection::Cell {
        row_id: s.data.rows[nr].id.clone(),
        field_id: s.data.fields[nc].id.clone(),
    });
    Task::none()
}

// ===== 样式 =====

fn style_toggle_patch(app: &App, key: tv::StyleKey) -> ops::StylePatch {
    let Some(s) = app.table.as_ref() else { return ops::StylePatch::default() };
    let Some(region) = ops::region_of(s.view.selection.as_ref(), &s.data.fields, &s.data.rows) else {
        return ops::StylePatch::default();
    };
    let summary = ops::style_summary(&s.data.fields, &s.data.rows, &region);
    // 全开 → 清除；mixed / 全关 → 统一置位
    let on = |state: &ops::OptState<bool>| matches!(state, ops::OptState::Value(Some(true)));
    let set = |on_now: bool| ops::KeyOp::Set(if on_now { None } else { Some(true) });
    match key {
        tv::StyleKey::Bold => ops::StylePatch { b: set(on(&summary.bold)), ..Default::default() },
        tv::StyleKey::Italic => ops::StylePatch { i: set(on(&summary.italic)), ..Default::default() },
        tv::StyleKey::Underline => ops::StylePatch { u: set(on(&summary.underline)), ..Default::default() },
        tv::StyleKey::Strike => ops::StylePatch { s: set(on(&summary.strike)), ..Default::default() },
    }
}

fn clear_format_patch() -> ops::StylePatch {
    ops::StylePatch {
        b: ops::KeyOp::Set(None),
        i: ops::KeyOp::Set(None),
        u: ops::KeyOp::Set(None),
        s: ops::KeyOp::Set(None),
        color: ops::KeyOp::Set(None),
        bg: ops::KeyOp::Set(None),
        font: ops::KeyOp::Set(None),
        size: ops::KeyOp::Set(None),
    }
}

fn apply_style(app: &mut App, patch: ops::StylePatch) -> Task<Message> {
    with_session(app, |s| {
        let Some(region) = ops::region_of(s.view.selection.as_ref(), &s.data.fields, &s.data.rows) else {
            return;
        };
        push_undo(s);
        if ops::apply_style_patch(&s.data.fields, &mut s.data.rows, &region, &patch) {
            s.touch();
        } else {
            drop_undo(s);
        }
    });
    Task::none()
}

// ===== 图片管线 =====

fn pick_images(row_id: String, field_id: String) -> Task<Message> {
    Task::perform(
        async move {
            let files = rfd::AsyncFileDialog::new()
                .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif"])
                .pick_files()
                .await
                .unwrap_or_default();
            let mut picked = Vec::new();
            for f in files {
                let bytes = f.read().await;
                picked.push((f.file_name(), bytes));
            }
            Message::Table(tv::TableMsg::ImagesPicked { row_id, field_id, files: picked })
        },
        |m| m,
    )
}

/// 选图入库：逐张生成唯一附件名并经写盘队列落字节，全部落地后一次性写入单元格
/// （整批一步撤销；超限 / 非图片文件预检拒绝计数）。空间模式走服务端字节上传。
fn import_images(app: &mut App, row_id: String, field_id: String, files: Vec<(String, Vec<u8>)>) -> Task<Message> {
    let Some(s) = app.table.as_ref() else { return Task::none() };
    let table_id = s.data.id.clone();
    // 预检共用：超限 / 非法名计数，产出上传任务清单
    let mut skipped = 0usize;
    let mut jobs: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, bytes) in files {
        if bytes.len() > tv::IMAGE_MAX_BYTES {
            skipped += 1;
            continue;
        }
        match vault::table_image_rel_path(&table_id, &name) {
            Ok(rel) => jobs.push((rel, bytes)),
            Err(_) => skipped += 1,
        }
    }
    if jobs.is_empty() {
        if skipped > 0 {
            app.notify(NoticeLevel::Warning, format!("有 {skipped} 个文件未导入"));
        }
        return Task::none();
    }
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        let Some(session) = space_actions::session_of(app) else { return Task::none() };
        if !session.role.can_write() {
            app.notify(NoticeLevel::Warning, "该空间为只读身份，无法上传图片");
            return Task::none();
        }
        let client = session.client;
        let space_id = session.space_id;
        return Task::perform(
            async move {
                let mut rels = Vec::new();
                let mut failed = skipped;
                for (rel, bytes) in jobs {
                    match client.put_file_bytes(&space_id, &rel, &bytes).await {
                        Ok(()) => rels.push(rel),
                        Err(_) => failed += 1,
                    }
                }
                Message::Table(tv::TableMsg::ImagesWritten { row_id, field_id, rels, failed })
            },
            |m| m,
        );
    }
    let Some(root) = app.vault.root().ok() else { return Task::none() };
    let queue = app.vault.writes.clone();
    Task::perform(
        async move {
            let mut rels = Vec::new();
            let mut failed = skipped;
            for (rel, bytes) in jobs {
                match vault::safe_join(&root, &rel.clone(), true) {
                    Ok(path) => {
                        if queue.write_bytes(path, bytes).await.is_ok() {
                            rels.push(rel);
                        } else {
                            failed += 1;
                        }
                    }
                    Err(_) => failed += 1,
                }
            }
            Message::Table(tv::TableMsg::ImagesWritten { row_id, field_id, rels, failed })
        },
        |m| m,
    )
}

/// 字段当前选项列表（缺省 = 空）。
fn current_options(s: &tv::TableSession, field_id: &str) -> Vec<String> {
    s.data
        .fields
        .iter()
        .find(|f| f.id == field_id)
        .and_then(|f| f.options.clone())
        .unwrap_or_default()
}

fn row_images_of(s: &tv::TableSession, row_id: &str, field_id: &str) -> Option<Vec<String>> {
    let row = s.data.rows.iter().find(|r| r.id == row_id)?;
    let v = row.values.get(field_id)?;
    Some(ops::image_entries(v).into_iter().map(str::to_string).collect())
}

fn row_images_len(s: &tv::TableSession, row_id: &str, field_id: &str) -> usize {
    s.data
        .rows
        .iter()
        .find(|r| r.id == row_id)
        .and_then(|r| r.values.get(field_id))
        .map(|v| ops::image_entries(v).len())
        .unwrap_or(0)
}

fn write_row_images(s: &mut tv::TableSession, row_id: &str, field_id: &str, images: Vec<String>) {
    let Some(row) = s.data.rows.iter_mut().find(|r| r.id == row_id) else { return };
    row.values.insert(field_id.to_string(), serde_json::json!({ "images": images }));
}

/// 提交图片拖拽顺序：命中槽位重排（槽位下标 = 布局携带的真实图片下标）。
fn commit_image_drag(s: &mut tv::TableSession, drag: tv::ImageDrag) {
    let Some(images) = row_images_of(s, &drag.row_id, &drag.field_id) else { return };
    let Some((r, c)) = s.cell_index(&drag.row_id, &drag.field_id) else { return };
    let g = tv::geo(s, s.view.canvas_size);
    let cell = tv::cell_rect(&g, r, c);
    let v = match s.data.rows[r].values.get(&s.data.fields[c].id) {
        Some(v) => v.clone(),
        None => return,
    };
    let idx = s
        .view
        .carousel
        .get(&(drag.row_id.clone(), drag.field_id.clone()))
        .copied()
        .unwrap_or(0);
    let layout = tv::image_layout_of(cell, &v, s.view.zoom, idx);
    let slots: Vec<(usize, &Rectangle)> = if layout.tiles.is_empty() {
        layout.thumbs.iter().map(|(i, _, r)| (*i, r)).collect()
    } else {
        layout.tiles.iter().map(|(i, _, r)| (*i, r)).collect()
    };
    if drag.from >= images.len() {
        return;
    }
    let mut target = drag.from;
    for (img_idx, rect) in &slots {
        if rect.contains(drag.pos) {
            target = *img_idx;
            break;
        }
    }
    if target == drag.from {
        return;
    }
    push_undo(s);
    let mut next = images;
    let item = next.remove(drag.from);
    next.insert(target, item);
    write_row_images(s, &drag.row_id, &drag.field_id, next);
    s.touch();
}

/// 长按激活检查（UiTick 心跳驱动）：缩略图按压超 400ms 未移动 = 进入图片拖拽排序
/// （主图长按不激活拖拽——主图干净松手即开灯箱，长按误触会吞掉灯箱）。
pub fn check_long_press(app: &mut App) {
    let Some(s) = app.table.as_mut() else { return };
    let press_info = s
        .view
        .press
        .as_ref()
        .and_then(|p| p.img.as_ref().map(|i| (i.row_id.clone(), i.field_id.clone(), i.kind, i.at)));
    let Some((row_id, field_id, kind, at)) = press_info else { return };
    if !matches!(kind, tv::ImagePressKind::Thumb(_)) {
        return;
    }
    if s.view.press.as_ref().is_some_and(|p| p.active) {
        return;
    }
    if at.elapsed().as_millis() < tv::IMAGE_LONG_PRESS_MS {
        return;
    }
    s.view.press.as_mut().expect("press 已判存在").active = true;
    let rel = row_images_of(s, &row_id, &field_id)
        .and_then(|list| match kind {
            tv::ImagePressKind::Thumb(i) => list.get(i).cloned(),
            tv::ImagePressKind::Main => None,
        })
        .unwrap_or_default();
    let from = match kind {
        tv::ImagePressKind::Thumb(i) => i,
        tv::ImagePressKind::Main => 0,
    };
    let pos = s.view.press.as_ref().map(|p| p.start).unwrap_or(Point::ORIGIN);
    s.view.img_drag = Some(tv::ImageDrag { row_id, field_id, rel, from, pos });
}

/// 灯箱当前图的仓库相对路径（越界 / 图已删 = None）。
fn lightbox_rel(s: &tv::TableSession, lb: &tv::Lightbox) -> Option<String> {
    let entries = s
        .data
        .rows
        .iter()
        .find(|r| r.id == lb.row_id)?
        .values
        .get(&lb.field_id)
        .map(|v| ops::image_entries(v))?;
    entries.get(lb.index).map(|rel| rel.to_string())
}

/// 下载灯箱当前图到系统 Downloads（文件名 = 附件名；重名自动加序号）。
fn download_lightbox_image(app: &App) -> Task<Message> {
    let Some(s) = app.table.as_ref() else { return Task::none() };
    let Some(lb) = s.view.lightbox.clone() else { return Task::none() };
    let Some(rel) = lightbox_rel(s, &lb) else { return Task::none() };
    // 空间模式：图片字节走服务端读取后落下载目录
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        let Some(session) = space_actions::session_of(app) else { return Task::none() };
        let client = session.client;
        let space_id = session.space_id;
        return Task::perform(
            async move {
                let bytes = client.get_file_bytes(&space_id, &rel).await.map_err(|e| e.to_string())?;
                let name = std::path::Path::new(&rel)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "图片.png".into());
                vault::save_image_to_downloads(&name, &bytes)
            },
            |r| Message::Table(tv::TableMsg::Downloaded(r)),
        );
    }
    let Some(root) = app.vault.root().ok() else { return Task::none() };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let path = vault::safe_join(&root, &rel.clone(), false)?;
                let bytes = std::fs::read(&path).map_err(|e| format!("读取图片失败：{e}"))?;
                let name = std::path::Path::new(&rel)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "图片.png".into());
                vault::save_image_to_downloads(&name, &bytes)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        },
        |r| Message::Table(tv::TableMsg::Downloaded(r)),
    )
}

/// 复制灯箱当前图到系统剪贴板（原图全分辨率；动图取首帧）。
fn copy_lightbox_image(app: &App) -> Task<Message> {
    let Some(s) = app.table.as_ref() else { return Task::none() };
    let Some(lb) = s.view.lightbox.clone() else { return Task::none() };
    let Some(rel) = lightbox_rel(s, &lb) else { return Task::none() };
    // 遗留内嵌 dataURL：解码即用
    if rel.starts_with("data:") {
        return Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let bytes = atelyx_core::table_xlsx::decode_data_url(&rel)
                        .ok_or_else(|| "图片数据解码失败".to_string())?;
                    copy_image_bytes(&bytes)
                })
                .await
                .map_err(|e| e.to_string())?
            },
            |r| Message::Table(tv::TableMsg::Copied(r)),
        );
    }
    // 空间模式：图片字节走服务端读取
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        let Some(session) = space_actions::session_of(app) else { return Task::none() };
        let client = session.client;
        let space_id = session.space_id;
        return Task::perform(
            async move {
                let bytes = client
                    .get_file_bytes(&space_id, &rel)
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::task::spawn_blocking(move || copy_image_bytes(&bytes))
                    .await
                    .map_err(|e| e.to_string())?
            },
            |r| Message::Table(tv::TableMsg::Copied(r)),
        );
    }
    let Some(root) = app.vault.root().ok() else { return Task::none() };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let path = vault::safe_join(&root, &rel, false)?;
                let bytes = std::fs::read(&path).map_err(|e| format!("读取图片失败：{e}"))?;
                copy_image_bytes(&bytes)
            })
            .await
            .map_err(|e| e.to_string())?
        },
        |r| Message::Table(tv::TableMsg::Copied(r)),
    )
}

/// 解码为 RGBA 后写系统剪贴板（调用方在线程池上执行）。
fn copy_image_bytes(bytes: &[u8]) -> Result<(), String> {
    let pixels = image_thumb::decode_rgba(bytes)?;
    crate::image_clipboard::write_image(pixels.width, pixels.height, pixels.rgba)
}

/// 导出 xlsx（core 纯逻辑 + I/O；单图解析失败跳过不阻断）。空间模式图片在服务端，不传仓库根。
fn export_xlsx(app: &App, path: Option<std::path::PathBuf>) -> Task<Message> {
    let Some(s) = app.table.as_ref() else { return Task::none() };
    let Some(target) = path else { return Task::none() };
    let root = match app.repo_kind {
        crate::RepoKind::Space(_) => None,
        crate::RepoKind::Local => app.vault.root().ok(),
    };
    let table = s.data.clone();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                atelyx_core::table_xlsx::export_table_xlsx(&table, root.as_deref(), &target)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        },
        |r| Message::Table(tv::TableMsg::Exported(r)),
    )
}

// ===== 图片句柄缓存 =====

/// 确保可见区域的图片句柄已发起加载（滚动/载入后调用；失败集不重试）。
pub fn ensure_image_loads(app: &mut App) -> Option<Task<Message>> {
    let s = app.table.as_ref()?;
    let view = &s.view;
    if view.canvas_size.width <= 0.0 {
        return None;
    }
    let g = tv::geo(s, view.canvas_size);
    let data = &s.data;
    let (first, last) = g.visible_rows(data.rows.len());
    let mut rels: Vec<String> = Vec::new();
    for r in first..=last {
        let Some(row) = data.rows.get(r) else { continue };
        for f in &data.fields {
            if f.field_type != "image" {
                continue;
            }
            let Some(v) = row.values.get(&f.id) else { continue };
            for rel in ops::image_entries(v) {
                let rel = rel.to_string();
                if view.images.contains_key(&rel)
                    || view.image_inflight.contains(&rel)
                    || view.image_failed.contains(&rel)
                {
                    continue;
                }
                rels.push(rel);
            }
        }
    }
    if rels.is_empty() {
        return None;
    }
    let gen = app.epoch();
    let inflight = rels.clone();
    // 空间模式：逐张走服务端字节读取（无文件指纹，dataURL 同样解码即用）
    let task = if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        let session = space_actions::session_of(app)?;
        let client = session.client;
        let space_id = session.space_id;
        Task::perform(
            async move {
                let mut fetched: Vec<(String, Result<Vec<u8>, String>)> = Vec::new();
                for rel in rels {
                    let result = if rel.starts_with("data:") {
                        atelyx_core::table_xlsx::decode_data_url(&rel)
                            .ok_or_else(|| "图片数据解码失败".to_string())
                    } else {
                        client.get_file_bytes(&space_id, &rel).await.map_err(|e| e.to_string())
                    };
                    fetched.push((rel, result));
                }
                // 解码与缩放走线程池（不占异步执行线程）
                let results: ImageLoadBatch = tokio::task::spawn_blocking(move || {
                    fetched
                        .into_iter()
                        .map(|(rel, bytes)| (rel, (0, 0), bytes.and_then(loaded_image)))
                        .collect()
                })
                .await
                .unwrap_or_default();
                (gen, results)
            },
            |(gen, results)| Message::TableImagesLoaded(gen, results),
        )
    } else {
        let root = app.vault.root().ok()?;
        Task::perform(
        async move {
            let results: ImageLoadBatch =
                tokio::task::spawn_blocking(move || {
                    let mut out = Vec::new();
                    for rel in rels {
                        // 遗留 dataURL 内嵌图：解码即用（无文件指纹）；路径引用经 safe_join 读仓库附件
                        if rel.starts_with("data:") {
                            let result = atelyx_core::table_xlsx::decode_data_url(&rel)
                                .ok_or_else(|| "图片数据解码失败".to_string())
                                .and_then(loaded_image);
                            out.push((rel, (0, 0), result));
                            continue;
                        }
                        let Ok(path) = vault::safe_join(&root, &rel, false) else {
                            out.push((rel, (0, 0), Err("非法图片路径".to_string())));
                            continue;
                        };
                        let fp = std::fs::metadata(&path)
                            .ok()
                            .and_then(|m| {
                                Some((
                                    m.modified()
                                        .ok()?
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .ok()?
                                        .as_nanos(),
                                    m.len(),
                                ))
                            })
                            .unwrap_or((0, 0));
                        let result = std::fs::read(&path)
                            .map_err(|e| e.to_string())
                            .and_then(loaded_image);
                        out.push((rel, fp, result));
                    }
                    out
                })
                .await
                .unwrap_or_default();
            (gen, results)
        },
        |(gen, results)| Message::TableImagesLoaded(gen, results),
        )
    };
    if let Some(s) = app.table.as_mut() {
        for rel in inflight {
            s.view.image_inflight.insert(rel);
        }
    }
    Some(task)
}

/// 载入结果：原图字节（灯箱解码）+ 缩略图像素（单元格绘制）。解码与缩放在调用线程做。
fn loaded_image(bytes: Vec<u8>) -> Result<tv::LoadedImage, String> {
    let thumb = image_thumb::thumbnail(&bytes, image_thumb::THUMB_MAX_EDGE)?;
    let size = (thumb.source.0 as f32, thumb.source.1 as f32);
    Ok(tv::LoadedImage { bytes, size, thumb })
}
