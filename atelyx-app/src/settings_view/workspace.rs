//! 设置-工作区：启动场景选择（仓库级，写仓库 config 的 entrySceneId）。

use iced::widget::{column, container};
use iced::{Element, Length, Padding};

use crate::layout_view::PopupState;
use crate::settings_view::{group_head, setting_row};
use crate::ui::controls;
use crate::ui::palette as pal;
use crate::ui::scroll;
use crate::ui::text;
use crate::{App, Message, RepoKind};

pub fn view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let space_mode = matches!(app.repo_kind, RepoKind::Space(_));
    if app.vault_root.is_none() && !space_mode {
        return container(controls::empty_state(
            "panels-top-left",
            "未打开仓库",
            "打开仓库后配置",
            None,
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }
    // 场景 id 悬挂（已删除）原样展示，选择时由解析守卫忽略
    let current = app
        .entry_scene_id
        .clone()
        .map(|id| {
            app.ui.scenes
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.clone())
                .unwrap_or(id)
        })
        .unwrap_or_else(|| "跟随当前".into());
    let trigger = controls::dropdown_trigger(
        &current,
        160.0,
        (!space_mode).then(|| super::open_popup(app, window, PopupState::EntrySceneMenu)),
    );
    let mut col = column![].spacing(8).padding(Padding::ZERO.horizontal(20.0));
    if space_mode {
        col = col.push(
            text("协作空间下由服务端管理")
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
        );
    }
    col = col.push(group_head("启动", "", true));
    col = col.push(setting_row(
        "启动时切换场景",
        "进入仓库时切到指定场景（跟随当前 = 保持上次界面）",
        trigger,
    ));
    scroll::v(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
