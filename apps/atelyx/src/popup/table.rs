//! 表格弹层：单元格 / 字段 / 列 / 行 / 视图与格式。

use iced::Element;

use atelyx_core::open_files::DocKind;
use atelyx_core::plugin_runtime::ContributionSlot;

use crate::layout_view::{PopupOpen, PopupState};
use crate::ui::menu::MenuEntry;
use crate::{App, Message};

use super::common::{popup_hint, popup_text_input};

pub(super) fn entries<'a>(app: &'a App, state: &PopupState, window: iced::window::Id) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::TableCellMenu => {
            let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
                (0, MenuEntry::action_icon("复制", "copy", Message::Table(crate::table_view::TableMsg::Menu(Box::new(crate::table_view::TableMsg::Copy))))),
                (0, MenuEntry::action_icon("剪切", "scissors", Message::Table(crate::table_view::TableMsg::Menu(Box::new(crate::table_view::TableMsg::Cut))))),
                (0, MenuEntry::action_icon("粘贴", "clipboard-paste", Message::Table(crate::table_view::TableMsg::Menu(Box::new(crate::table_view::TableMsg::Paste))))),
                (0, MenuEntry::action_icon("格式", "type", Message::Table(crate::table_view::TableMsg::ShowStyleBar))),
            ];
            if let Some((field_id, rows)) = crate::table_ai::selected_field(app) {
                entries.push((0, MenuEntry::Divider));
                entries.push((
                    0,
                    MenuEntry::action_icon(
                        "更新",
                        "refresh",
                        Message::Table(crate::table_view::TableMsg::AiStartRows {
                            field_id: field_id.clone(),
                            rows,
                        }),
                    ),
                ));
                entries.push((
                    0,
                    MenuEntry::action_icon(
                        "AI 字段设置…",
                        "bot",
                        Message::Table(crate::table_view::TableMsg::AiOpenConfig { window, field_id }),
                    ),
                ));
            }
            entries
        }
        PopupState::TableFieldMenu { field_id } => {
            let fid = field_id.clone();
            let ftype = app.table.as_ref().and_then(|s| {
                s.data.fields.iter().find(|f| f.id == *field_id).map(|f| f.field_type.clone())
            });
            let mut entries = vec![
                (
                    0,
                    MenuEntry::action(
                        "重命名",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableFieldRename { field_id: fid.clone() },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ),
                (
                    0,
                    MenuEntry::Submenu { label: "字段类型".into(), icon: None, disabled: false, pane: 2 },
                ),
            ];
            if ftype.as_deref() == Some("singleSelect") {
                entries.push((
                    0,
                    MenuEntry::action(
                        "单选选项管理",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableOptionsEdit { field_id: fid.clone() },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ));
            }
            if ftype.as_deref() == Some("ai") {
                let running = app
                    .table
                    .as_ref()
                    .is_some_and(|s| s.view.ai.batch.is_some());
                entries.push((
                    0,
                    MenuEntry::action_icon(
                        "AI 字段设置…",
                        "bot",
                        Message::Table(crate::table_view::TableMsg::AiOpenConfig {
                            window,
                            field_id: fid.clone(),
                        }),
                    ),
                ));
                entries.push((
                    0,
                    MenuEntry::Submenu { label: "生成".into(), icon: Some("bot".into()), disabled: false, pane: 4 },
                ));
                entries.push((
                    0,
                    MenuEntry::action(
                        "转为文本字段",
                        Message::Table(crate::table_view::TableMsg::RetypeField {
                            field_id: fid.clone(),
                            ftype: "text".into(),
                        }),
                    ),
                ));
                // pane 4：按筛选口径生成 + 停止
                const AI_GEN: [(&str, atelyx_core::table_ai::AiRowFilter); 4] = [
                    ("整列生成", atelyx_core::table_ai::AiRowFilter::All),
                    ("仅空值", atelyx_core::table_ai::AiRowFilter::Empty),
                    ("仅待更新", atelyx_core::table_ai::AiRowFilter::Stale),
                    ("仅失败项", atelyx_core::table_ai::AiRowFilter::Failed),
                ];
                for (label, filter) in AI_GEN {
                    entries.push((
                        4,
                        MenuEntry::action(
                            label,
                            Message::Table(crate::table_view::TableMsg::AiStart {
                                field_id: fid.clone(),
                                filter,
                            }),
                        ),
                    ));
                }
                if running {
                    entries.push((
                        4,
                        MenuEntry::action("停止生成", Message::Table(crate::table_view::TableMsg::AiStop)),
                    ));
                }
            }
            entries.push((0, MenuEntry::Divider));
            entries.push((
                0,
                MenuEntry::Submenu { label: "删除字段".into(), icon: None, disabled: false, pane: 3 },
            ));
            // pane 2：类型列表；pane 3：删除确认
            const FTYPES: [(&str, &str); 6] = [
                ("text", "文本"),
                ("number", "数字"),
                ("duration", "时长（秒）"),
                ("singleSelect", "单选"),
                ("image", "图片"),
                ("ai", "AI 字段"),
            ];
            for (ft, label) in FTYPES {
                let is_current = ftype.as_deref() == Some(ft);
                entries.push((
                    2,
                    MenuEntry::Action {
                        label: label.to_string(),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::RetypeField {
                            field_id: fid.clone(),
                            ftype: ft.to_string(),
                        })),
                        note: is_current.then_some("当前"),
                        hint: None,
                    },
                ));
            }
            entries.push((3, MenuEntry::action("确认删除（清空该列全部值）", Message::Table(crate::table_view::TableMsg::DeleteField(fid.clone())))));
            entries.push((3, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))));
            entries
        }
        PopupState::TableFieldRename { field_id } => {
            let fid = field_id.clone();
            let current = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id))
                .map(|f| f.name.clone())
                .unwrap_or_default();
            let draft = if app.table_popup_draft.is_empty() { current.clone() } else { app.table_popup_draft.clone() };
            vec![
                (0, MenuEntry::Custom(table_popup_input(&draft), 30.0)),
                (
                    0,
                    MenuEntry::action("重命名", Message::Table(crate::table_view::TableMsg::RenameField {
                        field_id: fid,
                        name: draft,
                    })),
                ),
                (0, MenuEntry::Custom(popup_hint(&format!("当前：{current}")), 18.0)),
            ]
        }
        PopupState::TableOptionsEdit { field_id } => {
            let fid = field_id.clone();
            let current = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id))
                .and_then(|f| f.options.clone())
                .unwrap_or_default();
            let draft = app.table_popup_draft.clone();
            // 逐项维护：输入 + 追加；已有选项逐条删除（保持弹层打开，外点关闭）
            let mut entries = vec![
                (0, MenuEntry::Custom(table_popup_input(&draft), 30.0)),
                (
                    0,
                    MenuEntry::action("添加选项", Message::Table(crate::table_view::TableMsg::OptionsAdd {
                        field_id: fid.clone(),
                        name: draft,
                    })),
                ),
                (0, MenuEntry::Divider),
            ];
            for (i, o) in current.iter().enumerate() {
                entries.push((
                    0,
                    MenuEntry::Action {
                        label: o.clone(),
                        icon: Some("x".into()),
                        danger: true,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::OptionsRemove {
                            field_id: fid.clone(),
                            index: i,
                        })),
                        note: None,
                        hint: None,
                    },
                ));
            }
            if current.is_empty() {
                entries.push((0, MenuEntry::Custom(popup_hint("暂无选项"), 18.0)));
            }
            entries
        }
        PopupState::TableColumnMenu { field_id } => {
            let fid = field_id.clone();
            let entries = vec![
                (
                    0,
                    MenuEntry::action_icon(
                        "列宽自适应",
                        "columns-3",
                        Message::Table(crate::table_view::TableMsg::ColumnAutoWidth(Some(fid.clone()))),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "格式",
                        "type",
                        Message::Table(crate::table_view::TableMsg::ShowStyleBar),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "左侧插入字段",
                        "arrow-left",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableColumnInsert { field_id: fid.clone(), side: -1 },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "右侧插入字段",
                        "arrow-right",
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::TableColumnInsert { field_id: fid.clone(), side: 1 },
                                min_width: 0.0,
                            },
                        ),
                    ),
                ),
            ];
            entries
        }
        PopupState::TableColumnInsert { field_id, side } => {
            let side = *side;
            let draft = app.table_popup_draft.clone();
            let idx = app.table.as_ref().and_then(|s| {
                s.data.fields.iter().position(|f| f.id == *field_id).map(|i| if side < 0 { i } else { i + 1 })
            });
            vec![
                (0, MenuEntry::Custom(table_popup_input(&draft), 30.0)),
                (
                    0,
                    MenuEntry::action("添加", Message::Table(crate::table_view::TableMsg::InsertField {
                        index: idx,
                        name: draft,
                    })),
                ),
                (0, MenuEntry::action("取消", Message::Ui(crate::ui::UiMsg::PopupClose { window }))),
            ]
        }
        PopupState::TableRowMenu { row_id } => {
            let rid = row_id.clone();
            vec![
                (
                    0,
                    MenuEntry::action_icon("复制行", "columns-3", Message::Table(crate::table_view::TableMsg::DuplicateRow(rid.clone()))),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "上方插入空行",
                        "arrow-up",
                        Message::Table(crate::table_view::TableMsg::InsertRow { row_id: rid.clone(), side: -1 }),
                    ),
                ),
                (
                    0,
                    MenuEntry::action_icon(
                        "下方插入空行",
                        "arrow-down",
                        Message::Table(crate::table_view::TableMsg::InsertRow { row_id: rid.clone(), side: 1 }),
                    ),
                ),
                (
                    0,
                    MenuEntry::action("上移", Message::Table(crate::table_view::TableMsg::MoveRow { row_id: rid.clone(), delta: -1 })),
                ),
                (
                    0,
                    MenuEntry::action("下移", Message::Table(crate::table_view::TableMsg::MoveRow { row_id: rid.clone(), delta: 1 })),
                ),
                (
                    0,
                    MenuEntry::action_icon("行高自适应", "space-around", Message::Table(crate::table_view::TableMsg::RowHeightAuto(Some(rid.clone())))),
                ),
                (
                    0,
                    MenuEntry::action_icon("格式", "type", Message::Table(crate::table_view::TableMsg::ShowStyleBar)),
                ),
                (0, MenuEntry::Divider),
                (
                    0,
                    MenuEntry::danger("删除行", "bin", Message::Table(crate::table_view::TableMsg::DeleteRows(vec![rid]))),
                ),
            ]
        }
        PopupState::TableAddField => {
            let draft = app.table_popup_draft.clone();
            let mut entries = vec![(0, MenuEntry::Custom(table_popup_input(&draft), 30.0))];
            const FTYPES: [(&str, &str); 6] = [
                ("text", "文本"),
                ("number", "数字"),
                ("duration", "时长（秒）"),
                ("singleSelect", "单选"),
                ("image", "图片"),
                ("ai", "AI"),
            ];
            for (ft, label) in FTYPES {
                let on_press = if ft == "ai" {
                    Message::Table(crate::table_view::TableMsg::AiCreateField {
                        window,
                        name: draft.clone(),
                    })
                } else {
                    Message::Table(crate::table_view::TableMsg::AddField {
                        name: draft.clone(),
                        ftype: ft.to_string(),
                    })
                };
                entries.push((
                    0,
                    MenuEntry::Action {
                        label: format!("添加{label}字段"),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(on_press),
                        note: None,
                        hint: None,
                    },
                ));
            }
            entries
        }
        PopupState::TableStatMenu { field_id } => {
            let fid = field_id.clone();
            let field = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id).cloned());
            let mut entries = vec![(
                0,
                MenuEntry::Action {
                    label: "无".into(),
                    icon: None,
                    danger: false,
                    disabled: false,
                    on_press: Some(Message::Table(crate::table_view::TableMsg::SetCalc {
                        field_id: fid.clone(),
                        calc: None,
                    })),
                    note: field.as_ref().and_then(|f| f.calc_type.as_deref()).is_none().then_some("当前"),
                    hint: None,
                },
            )];
            if let Some(f) = field {
                for calc in atelyx_core::table_ops::calc_types_for(&f.field_type) {
                    let is_current = f.calc_type.as_deref() == Some(*calc);
                    entries.push((
                        0,
                        MenuEntry::Action {
                            label: atelyx_core::table_ops::calc_type_label(calc).to_string(),
                            icon: None,
                            danger: false,
                            disabled: false,
                            on_press: Some(Message::Table(crate::table_view::TableMsg::SetCalc {
                                field_id: fid.clone(),
                                calc: Some(calc.to_string()),
                            })),
                            note: is_current.then_some("当前"),
                            hint: None,
                        },
                    ));
                }
            }
            entries
        }
        PopupState::TableSelectAllMenu => vec![
            (
                0,
                MenuEntry::action_icon("列宽自适应", "columns-3", Message::Table(crate::table_view::TableMsg::ColumnAutoWidth(None))),
            ),
            (
                0,
                MenuEntry::action_icon("行高自适应", "space-around", Message::Table(crate::table_view::TableMsg::RowHeightAuto(None))),
            ),
            (
                0,
                MenuEntry::action_icon("格式", "type", Message::Table(crate::table_view::TableMsg::ShowStyleBar)),
            ),
        ],
        PopupState::TableMoreMenu => {
            let table_open = crate::open_route::doc_path(app, &DocKind::Table);
            vec![
                (
                    0,
                    MenuEntry::Action {
                        label: "历史记录".into(),
                        icon: Some("history".into()),
                        danger: false,
                        disabled: table_open.is_none(),
                        on_press: table_open.map(|file| {
                            Message::History(crate::history_browser::HistoryBrowserMsg::Open {
                                window,
                                kind: atelyx_core::history::HistoryKind::Table,
                                file,
                            })
                        }),
                        note: None,
                        hint: None,
                    },
                ),
                (0, MenuEntry::action_icon("导出 xlsx", "download", Message::Table(crate::table_view::TableMsg::ExportXlsxPick))),
            ]
        }
        PopupState::TableSizeMenu => {
            const SIZES: [f64; 7] = [12.0, 13.0, 14.0, 16.0, 18.0, 20.0, 24.0];
            let mut entries = vec![(
                0,
                MenuEntry::Action {
                    label: "默认".into(),
                    icon: None,
                    danger: false,
                    disabled: false,
                    on_press: Some(Message::Table(crate::table_view::TableMsg::SetSize(None))),
                    note: Some("清除"),
                    hint: None,
                },
            )];
            for s in SIZES {
                entries.push((
                    0,
                    MenuEntry::Action {
                        label: format!("{}", s as i32),
                        icon: None,
                        danger: false,
                        disabled: false,
                        on_press: Some(Message::Table(crate::table_view::TableMsg::SetSize(Some(s)))),
                        note: None,
                        hint: None,
                    },
                ));
            }
            entries
        }
        PopupState::TableSelectMenu { row_id, field_id } => {
            let (rid, fid) = (row_id.clone(), field_id.clone());
            let current = app
                .table
                .as_ref()
                .and_then(|s| s.data.rows.iter().find(|r| r.id == *row_id))
                .and_then(|r| r.values.get(field_id))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let options = app
                .table
                .as_ref()
                .and_then(|s| s.data.fields.iter().find(|f| f.id == *field_id))
                .and_then(|f| f.options.clone())
                .unwrap_or_default();
            let select = |rid: String, fid: String, value: Option<String>| {
                Message::Table(crate::table_view::TableMsg::SelectOption { row_id: rid, field_id: fid, value })
            };
            let mut entries = vec![(
                0,
                MenuEntry::check("未选择", current.is_none(), select(rid.clone(), fid.clone(), None)),
            )];
            for o in options {
                let is_current = current.as_deref() == Some(o.as_str());
                entries.push((
                    0,
                    MenuEntry::check(o.clone(), is_current, select(rid.clone(), fid.clone(), Some(o))),
                ));
            }
            entries
        }
        PopupState::TableLightboxMenu => vec![
            (
                0,
                MenuEntry::action_icon("复制图片", "copy", Message::Table(crate::table_view::TableMsg::LightboxCopy)),
            ),
            (
                0,
                MenuEntry::action_icon("下载图片", "download", Message::Table(crate::table_view::TableMsg::LightboxDownload)),
            ),
        ],
        PopupState::TableViewMenu => {
            // 视图选择：内置网格 + 各插件注册的表格视图（选中项带「当前」，切换即挂载 / 分离面）
            let active = app
                .table
                .as_ref()
                .and_then(|t| t.view.active_view.as_deref())
                .map(str::to_string);
            let grid = Message::Table(crate::table_view::TableMsg::SetView(None));
            let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![(
                0,
                MenuEntry::check("表格", active.is_none(), grid),
            )];
            let views: Vec<(String, String)> = app
                .registry
                .views_in(ContributionSlot::TableView)
                .map(|entry| (entry.label.clone(), entry.id.clone()))
                .collect();
            if !views.is_empty() {
                entries.push((0, MenuEntry::Divider));
                for (label, kind) in views {
                    let pick = Message::Table(crate::table_view::TableMsg::SetView(Some(kind.clone())));
                    let selected = active.as_deref() == Some(kind.as_str());
                    entries.push((0, MenuEntry::check(label, selected, pick)));
                }
            }
            entries
        }
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::TableCellMenu => 152.0,
        PopupState::TableFieldMenu { .. } => 176.0,
        PopupState::TableFieldRename { .. } => 200.0,
        PopupState::TableOptionsEdit { .. } => 208.0,
        PopupState::TableColumnMenu { .. } => 176.0,
        PopupState::TableColumnInsert { .. } => 200.0,
        PopupState::TableRowMenu { .. } => 144.0,
        PopupState::TableAddField => 192.0,
        PopupState::TableStatMenu { .. } => 128.0,
        PopupState::TableSelectAllMenu => 160.0,
        PopupState::TableMoreMenu => 144.0,
        PopupState::TableSizeMenu => 96.0,
        PopupState::TableSelectMenu { .. } => 144.0,
        PopupState::TableLightboxMenu => 128.0,
        PopupState::TableViewMenu => 96.0,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}

/// 表格弹层内联输入（草稿 = 表格弹层草稿；提交由菜单项动作携带）。
fn table_popup_input(draft: &str) -> Element<'static, Message> {
    popup_text_input("名称", draft, |v| Message::Table(crate::table_view::TableMsg::PopupDraft(v)))
}
