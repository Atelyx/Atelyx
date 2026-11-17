//! 弹层内容分派：按变体家族路由到各域渲染体，宽度与渲染体同域定义。

mod ai;
mod canvas;
mod common;
mod document;
mod file;
mod menu;
mod modal;
mod settings;
mod space;
mod table;

use iced::Element;

use crate::layout_view::PopupState;
use crate::ui::menu::MenuEntry;
use crate::{App, Message};

/// 弹层内容构造：按形态分派到所属域，返回（元素, 估算高度）。
pub(crate) fn element<'a>(
    app: &'a App,
    state: &PopupState,
    pane: usize,
    window: iced::window::Id,
) -> (Element<'a, Message>, f32) {
    let entries: Vec<(usize, MenuEntry<'a, Message>)> = match state {
        PopupState::SceneSelector { .. }
        | PopupState::LayoutTabMenu { .. }
        | PopupState::PanelMenu { .. }
        | PopupState::TabContext { .. }
        | PopupState::ViewPicker { .. }
        | PopupState::DetachedPanelMenu { .. }
        | PopupState::DetachedTabContext { .. } => menu::entries(app, state),
        PopupState::SortMenu
        | PopupState::FileMenu { .. }
        | PopupState::VaultMenu { .. } => file::entries(app, state, window),
        PopupState::EntrySceneMenu
        | PopupState::FontPicker { .. }
        | PopupState::ColorPicker { .. } => settings::entries(app, state, window),
        PopupState::SpaceMenu
        | PopupState::SpaceLogin { .. }
        | PopupState::SpaceCreate
        | PopupState::SpaceJoin
        | PopupState::SpaceRowMenu { .. }
        | PopupState::SpaceMembers
        | PopupState::SpaceInvite => space::entries(app, state, window),
        PopupState::NoteMenu
        | PopupState::PropTypeMenu { .. }
        | PopupState::SelectionMenu { .. } => document::entries(app, state, window),
        PopupState::TableCellMenu
        | PopupState::TableFieldMenu { .. }
        | PopupState::TableFieldRename { .. }
        | PopupState::TableOptionsEdit { .. }
        | PopupState::TableColumnMenu { .. }
        | PopupState::TableColumnInsert { .. }
        | PopupState::TableRowMenu { .. }
        | PopupState::TableAddField
        | PopupState::TableStatMenu { .. }
        | PopupState::TableSelectAllMenu
        | PopupState::TableMoreMenu
        | PopupState::TableSizeMenu
        | PopupState::TableSelectMenu { .. }
        | PopupState::TableLightboxMenu
        | PopupState::TableViewMenu => table::entries(app, state, window),
        PopupState::CanvasCardMenu { .. }
        | PopupState::CanvasGroupMenu { .. }
        | PopupState::CanvasMultiMenu
        | PopupState::CanvasBlankMenu { .. }
        | PopupState::CanvasGroupRename { .. } => canvas::entries(app, state),
        PopupState::AiModelMenu
        | PopupState::ChatAgentMenu { .. }
        | PopupState::ChatPlusMenu { .. }
        | PopupState::ChatModelMenu { .. }
        | PopupState::AiNamingMenu
        | PopupState::AiAgentPromptMenu
        | PopupState::AiProviderDeleteMenu
        | PopupState::AiAgentDeleteMenu { .. }
        | PopupState::AiSearchProviderMenu => ai::entries(app, state, pane, window),
        // 居中模态不经锚点渲染（with_overlays 分流到各自 overlay）
        PopupState::HistoryBrowser { .. } | PopupState::TableAiField { .. } => Vec::new(),
    };
    crate::ui::menu::pane_view(entries, pane, window)
}

/// 弹层宽度：与各域渲染体就近定义（popup/<域>::width）。
pub(crate) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::SceneSelector { .. }
        | PopupState::LayoutTabMenu { .. }
        | PopupState::PanelMenu { .. }
        | PopupState::TabContext { .. }
        | PopupState::ViewPicker { .. }
        | PopupState::DetachedPanelMenu { .. }
        | PopupState::DetachedTabContext { .. } => menu::width(state),
        PopupState::SortMenu
        | PopupState::FileMenu { .. }
        | PopupState::VaultMenu { .. } => file::width(state),
        PopupState::EntrySceneMenu
        | PopupState::FontPicker { .. }
        | PopupState::ColorPicker { .. } => settings::width(state),
        PopupState::SpaceMenu
        | PopupState::SpaceLogin { .. }
        | PopupState::SpaceCreate
        | PopupState::SpaceJoin
        | PopupState::SpaceRowMenu { .. }
        | PopupState::SpaceMembers
        | PopupState::SpaceInvite => space::width(state),
        PopupState::NoteMenu
        | PopupState::PropTypeMenu { .. }
        | PopupState::SelectionMenu { .. } => document::width(state),
        PopupState::TableCellMenu
        | PopupState::TableFieldMenu { .. }
        | PopupState::TableFieldRename { .. }
        | PopupState::TableOptionsEdit { .. }
        | PopupState::TableColumnMenu { .. }
        | PopupState::TableColumnInsert { .. }
        | PopupState::TableRowMenu { .. }
        | PopupState::TableAddField
        | PopupState::TableStatMenu { .. }
        | PopupState::TableSelectAllMenu
        | PopupState::TableMoreMenu
        | PopupState::TableSizeMenu
        | PopupState::TableSelectMenu { .. }
        | PopupState::TableLightboxMenu
        | PopupState::TableViewMenu => table::width(state),
        PopupState::CanvasCardMenu { .. }
        | PopupState::CanvasGroupMenu { .. }
        | PopupState::CanvasMultiMenu
        | PopupState::CanvasBlankMenu { .. }
        | PopupState::CanvasGroupRename { .. } => canvas::width(state),
        PopupState::AiModelMenu
        | PopupState::ChatAgentMenu { .. }
        | PopupState::ChatPlusMenu { .. }
        | PopupState::ChatModelMenu { .. }
        | PopupState::AiNamingMenu
        | PopupState::AiAgentPromptMenu
        | PopupState::AiProviderDeleteMenu
        | PopupState::AiAgentDeleteMenu { .. }
        | PopupState::AiSearchProviderMenu => ai::width(state),
        PopupState::HistoryBrowser { .. }
        | PopupState::TableAiField { .. } => modal::width(state),
    }
}
