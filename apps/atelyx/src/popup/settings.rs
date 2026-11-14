//! 设置与选择器弹层：启动场景 / 字体 / 取色。

use crate::layout_view::{viewport_of, PopupState};
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::popup;
use crate::{App, Message};

use super::common::{popup_hint, popup_text_input};

pub(super) fn entries<'a>(app: &'a App, state: &PopupState, window: iced::window::Id) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::EntrySceneMenu => {
            let pick = |id: Option<String>| Message::EntryScenePick(id);
            let current = app.entry_scene_id.as_deref();
            let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![(
                0,
                MenuEntry::check("跟随当前", current.is_none(), pick(None)),
            )];
            for s in &app.ui.scenes {
                let is_current = current == Some(s.id.as_str());
                let (id, name) = (s.id.clone(), s.name.clone());
                entries.push((
                    0,
                    MenuEntry::check(name, is_current, pick(Some(id))),
                ));
            }
            entries
        }
        PopupState::FontPicker { for_table } => font_picker_entries(app, *for_table, window),
        PopupState::ColorPicker { target, hsv, hex } => {
            let (panel, height) = crate::color_picker_actions::panel(app, target, *hsv, hex);
            vec![(0, MenuEntry::Custom(panel, height))]
        }
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::EntrySceneMenu => 176.0,
        PopupState::FontPicker { .. } => 240.0,
        PopupState::ColorPicker { .. } => crate::ui::color_picker::PANEL_W,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}

/// 字体选择弹层条目：过滤输入 + 复位项 + 命中族清单（清单定高，超出走内滚动）。
fn font_picker_entries(
    app: &App,
    for_table: bool,
    window: iced::window::Id,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    const SEARCH_H: f32 = 30.0;
    /// 清单可视区行数上下限（再多也只给滚动区，不让弹层顶满屏幕）。
    const MIN_ROWS: usize = 2;
    const MAX_ROWS: usize = 16;

    let hits = crate::ui::font::filter(&app.font_picker_query);
    let current = if for_table { table_cell_font(app) } else { app.global.font_family.clone() };
    let resolved_current = current.as_deref().and_then(crate::ui::font::lookup);
    let reset = if for_table {
        Message::Table(crate::table_view::TableMsg::SetFont(None))
    } else {
        Message::InterfaceFontSet(None)
    };
    let reset_label = if for_table { "默认" } else { "跟随系统" };
    let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![
        (
            0,
            MenuEntry::Custom(
                popup_text_input("搜索字体…", &app.font_picker_query, Message::FontPickerQuery),
                SEARCH_H,
            ),
        ),
        (0, MenuEntry::Divider),
    ];
    let reset_entry = if current.is_none() {
        MenuEntry::Checked { label: reset_label.to_string(), on_press: reset }
    } else {
        MenuEntry::Action {
            label: reset_label.to_string(),
            icon: None,
            danger: false,
            disabled: false,
            on_press: Some(reset),
            note: for_table.then_some("清除"),
            hint: None,
        }
    };
    entries.push((0, reset_entry));
    if hits.is_empty() {
        entries.push((0, MenuEntry::Custom(popup_hint("没有匹配的字体"), 18.0)));
        return entries;
    }
    // 可视区高度 = 窗口高 − 弹层固定部分（上下内边距 + 搜索行 + 分隔线 + 复位行）− 阴影环 − 上下留白
    let chrome = pal::POPUP_PAD_Y * 2.0
        + SEARCH_H
        + pal::MENU_DIVIDER_H
        + pal::MENU_ITEM_H
        + popup::SHADOW_PAD_TOP
        + popup::SHADOW_PAD_BOTTOM
        + pal::VIEWPORT_GAP * 2.0;
    let budget = (viewport_of(app, window).height - chrome).max(MIN_ROWS as f32 * pal::MENU_ITEM_H);
    let list_h = (hits.len().min(MAX_ROWS) as f32 * pal::MENU_ITEM_H).min(budget);
    let rows = hits.into_iter().map(|hit| {
        let name = hit.name.clone();
        let pick = if for_table {
            Message::Table(crate::table_view::TableMsg::SetFont(Some(name.clone())))
        } else {
            Message::InterfaceFontSet(Some(name.clone()))
        };
        // 存量值可能是同族的另一语言写法（如 SimSun 与 宋体），按解析后的展示名认当前项
        if resolved_current.as_deref() == Some(name.as_str()) {
            MenuEntry::Checked { label: name, on_press: pick }
        } else {
            MenuEntry::action(name, pick)
        }
    });
    entries.push((0, MenuEntry::Scroll { entries: rows.collect(), height: list_h }));
    entries
}

/// 表格字体弹层的当前值：选中区统一字体（混合或未设 = 无当前项）。
fn table_cell_font(app: &App) -> Option<String> {
    use atelyx_core::table_ops as ops;
    let session = app.table.as_ref()?;
    let region = ops::region_of(session.view.selection.as_ref(), &session.data.fields, &session.data.rows)?;
    match ops::style_summary(&session.data.fields, &session.data.rows, &region).font {
        ops::OptState::Value(font) => font,
        ops::OptState::Mixed => None,
    }
}
