//! 正文与属性弹层：笔记菜单 / 属性类型 / 选区复制。

use crate::layout_view::PopupState;
use crate::ui::menu::MenuEntry;
use crate::{App, Message, NoteMode};

pub(super) fn entries<'a>(app: &'a App, state: &PopupState, window: iced::window::Id) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::NoteMenu => {
            let note_open = app.ui.last_note_file.is_some();
            // 只读身份：属性写入与历史回滚都改正文，菜单项直接禁用（不留下点击无反应的入口）
            let readonly = app.note_readonly();
            let readonly_hint = readonly.then(|| "当前身份为只读".to_string());
            vec![
                (
                    0,
                    MenuEntry::Action {
                        label: "添加笔记属性".into(),
                        icon: None,
                        danger: false,
                        disabled: !note_open || readonly,
                        on_press: (!readonly).then_some(Message::Props(
                            crate::properties_view::PropsMsg::StartAdd(
                                crate::properties_view::PropsSurface::Inline,
                            ),
                        )),
                        note: None,
                        hint: if !note_open {
                            Some("未打开笔记".to_string())
                        } else {
                            readonly_hint.clone()
                        },
                    },
                ),
                (
                    0,
                    MenuEntry::Action {
                        label: "历史记录".into(),
                        icon: Some("history".into()),
                        danger: false,
                        disabled: !note_open || readonly,
                        on_press: if readonly {
                            None
                        } else {
                            app.ui.last_note_file.clone().map(|file| {
                                Message::History(crate::history_browser::HistoryBrowserMsg::Open {
                                    window,
                                    kind: atelyx_core::history::HistoryKind::Note,
                                    file,
                                })
                            })
                        },
                        note: None,
                        hint: if !note_open {
                            Some("未打开笔记".to_string())
                        } else {
                            readonly_hint.clone()
                        },
                    },
                ),
            ]
        }
        PopupState::PropTypeMenu { key } => {
            // 固定顺序六类型；徽章键锁定标签（其余项禁用）
            use atelyx_core::frontmatter::{infer_type, PropType, Yaml};
            let row = match app.props.parsed.as_ref() {
                Some((Yaml::Map(pairs), true)) => pairs.iter().find(|(k, _)| k == key),
                _ => None,
            };
            let badge = row.is_some_and(|(k, _)| crate::properties_view::is_badge_key(k));
            let current = row.map(|(_, v)| infer_type(badge, v));
            const TYPES: [(PropType, &str, &str); 6] = [
                (PropType::Text, "type", "文本"),
                (PropType::Tag, "tag", "标签"),
                (PropType::Date, "calendar-days", "日期"),
                (PropType::Number, "hash", "数字"),
                (PropType::Boolean, "toggle-left", "布尔"),
                (PropType::List, "list", "列表"),
            ];
            TYPES
                .iter()
                .map(|(pt, icon_name, label)| {
                    // 标签类型只属于 tags：其余键没有 `#` 渲染语义，禁用避免误以为是标签
                    let locked = if badge { *pt != PropType::Tag } else { *pt == PropType::Tag };
                    let is_current = current == Some(*pt);
                    (
                        0,
                        MenuEntry::Action {
                            label: (*label).to_string(),
                            icon: Some(icon_name.to_string()),
                            danger: false,
                            disabled: locked,
                            on_press: (!locked)
                                .then(|| Message::Props(crate::properties_view::PropsMsg::ConvertType(key.clone(), *pt))),
                            note: is_current.then_some("当前"),
                            hint: locked.then(|| {
                                if badge { "tags 恒为标签" } else { "仅 tags 属性可用" }.to_string()
                            }),
                        },
                    )
                })
                .collect()
        }
        PopupState::SelectionMenu { target } => {
            let has_text = match target {
                crate::markdown_select::SelectionTarget::Note => {
                    app.note_mode == NoteMode::Read
                        && app.note_selection.is_some_and(|s| {
                            let (from, to) = s.range();
                            from < to
                        })
                }
                crate::markdown_select::SelectionTarget::Chat => {
                    app.chat.selection.is_some_and(|s| {
                        let (from, to) = s.range();
                        from < to
                    })
                }
            };
            vec![(
                0,
                MenuEntry::Action {
                    label: "复制".into(),
                    icon: Some("copy".into()),
                    danger: false,
                    disabled: !has_text,
                    on_press: has_text.then_some(Message::CopySelection(*target)),
                    note: None,
                    hint: None,
                },
            )]
        }
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::NoteMenu => 144.0,
        PopupState::SelectionMenu { .. } => 128.0,
        PopupState::PropTypeMenu { .. } => 128.0,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}
