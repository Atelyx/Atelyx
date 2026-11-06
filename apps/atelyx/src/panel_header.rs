//! 面板头部：宿主按插件声明的状态渲染面板顶部那一行工具条（状态胶囊 / 错误文本 /
//! 协作者胶囊 / 右侧文本 / 菜单项 / 视图下拉）。插件只喂数据，视觉与宿主控件同口径。

use iced::widget::{row, space};
use iced::{alignment, Element, Length, Padding};

use atelyx_core::plugin_runtime::{ContributionSlot, HeaderTone};

use crate::layout_view::{collab_pill_row, ellipsize, parse_hex_color, view_label};
use crate::ui::controls::{self, PillStatus};
use crate::ui::palette as pal;
use crate::ui::{menu::MenuEntry, text};
use crate::{App, Message};

/// 头部行高（与面板内其它工具条同档）。
const HEADER_H: f32 = 30.0;
/// 视图下拉触发器宽（固定宽防切视图时整行跳动）。
const VIEW_TRIGGER_W: f32 = 120.0;
/// 错误文本截断宽（超出由宿主截断，不撑破整行）。
const ERROR_MAX_W: f32 = 220.0;

/// 面板头部整行；该面板没有声明头部时返回 `None`（面板内容照旧占满）。
/// `target` = 头部归属的视图贡献 id，`slot` = 视图下拉写回的槽（两者在普通面板相同）。
pub(crate) fn header_row<'a>(
    app: &'a App,
    window: iced::window::Id,
    target: &str,
    slot: &str,
) -> Option<Element<'a, Message>> {
    let entry = app.panel_headers.get(target)?;
    // 插件已停用 / 未运行时不渲染它留下的头部（点击只会落到无会话的空转）
    if !app
        .plugins
        .list()
        .iter()
        .any(|plugin| plugin.id == entry.plugin_id && plugin.status == crate::PluginStatus::Running)
    {
        return None;
    }
    let state = &entry.state;
    // 模态插件浮层盖住这一行时，点击先交给浮层收起（不落到头部按钮）
    let modal = app
        .plugin_modal
        .as_ref()
        .is_some_and(|(plugin_id, _)| *plugin_id == entry.plugin_id);
    // 只有脏标记、没有可见内容的头部不占高度（画布只用它参与 flush 门控）。
    let mut visible = false;
    let mut bar = row![]
        .spacing(10)
        .padding(Padding::ZERO.horizontal(10.0))
        .height(Length::Fixed(HEADER_H))
        .align_y(alignment::Vertical::Center);
    if state.view_slot == Some(ContributionSlot::TableView) {
        visible = true;
        let active = app.panel_view.get(target).cloned().flatten();
        let label = view_label(app, active.as_deref().unwrap_or(target));
        bar = bar.push(
            text("视图：")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
        bar = bar.push(controls::dropdown_trigger(
            &label,
            VIEW_TRIGGER_W,
            Some(Message::PanelViewMenu {
                window,
                target: slot.to_string(),
            }),
        ));
    }
    if let Some(status) = &state.status {
        visible = true;
        bar = bar.push(controls::status_pill(status_tone(status.tone), status.text.clone()));
    }
    bar = bar.push(space().width(Length::Fill));
    if let Some(error) = &state.error {
        visible = true;
        bar = bar.push(
            text(ellipsize(error, ERROR_MAX_W, pal::fs_micro()))
                .size(pal::fs_micro())
                .color(pal::DANGER),
        );
    }
    if !state.pills.is_empty() {
        visible = true;
        let members: Vec<(iced::Color, String)> = state
            .pills
            .iter()
            .map(|pill| {
                (
                    parse_hex_color(&pill.color).unwrap_or(pal::TEXT_MUTED),
                    pill.text.clone(),
                )
            })
            .collect();
        bar = bar.push(collab_pill_row(&members));
    }
    for label in &state.texts {
        visible = true;
        bar = bar.push(
            text(label.clone())
                .size(pal::fs_caption())
                .color(pal::TEXT_SECONDARY),
        );
    }
    if !state.items.is_empty() {
        visible = true;
        bar = bar.push(controls::icon_button(
            "ellipsis",
            14.0,
            controls::ICON_MD - 8.0,
            controls::Variant::Ghost,
            Some(Message::PanelItemsMenu {
                window,
                target: target.to_string(),
            }),
            Some((window, "更多".to_string())),
        ));
    }
    if !visible && !modal {
        return None;
    }
    let row: Element<'_, Message> = bar.into();
    if modal {
        return Some(
            iced::widget::mouse_area(row)
                .on_press(Message::PluginPopupDismiss { window })
                .into(),
        );
    }
    Some(row)
}

/// 头部状态胶囊色调 → 宿主胶囊档位。
fn status_tone(tone: HeaderTone) -> PillStatus {
    match tone {
        HeaderTone::Normal => PillStatus::Info,
        HeaderTone::Pending => PillStatus::Pending,
        HeaderTone::Danger => PillStatus::Danger,
    }
}

/// 头部条目约定 id → 内置图标（协议条目不带图标，按 id 映射；未约定保留无图标）。
fn item_icon(id: &str) -> Option<&'static str> {
    match id {
        "history" => Some("history"),
        "export" => Some("download"),
        _ => None,
    }
}

/// 头部「⋯」菜单条目：宿主渲染，点击按条目 id 回发插件。
pub(crate) fn item_entries(app: &App, target: &str) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let Some(state) = app.panel_headers.get(target).map(|entry| &entry.state) else {
        return Vec::new();
    };
    state
        .items
        .iter()
        .map(|item| {
            (
                0,
                MenuEntry::Action {
                    label: item.label.clone(),
                    icon: item_icon(&item.id).map(str::to_string),
                    danger: false,
                    disabled: false,
                    note: None,
                    hint: None,
                    on_press: Some(Message::PanelHeaderAction {
                        target: target.to_string(),
                        item: item.id.clone(),
                    }),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::item_icon;

    #[test]
    fn header_items_map_to_builtin_icons() {
        assert_eq!(item_icon("history"), Some("history"));
        assert_eq!(item_icon("export"), Some("download"));
        assert_eq!(item_icon("unknown"), None);
    }
}
