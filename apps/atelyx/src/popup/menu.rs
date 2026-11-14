//! 工作区 chrome 弹层：场景选择 / 布局标签 / 面板与标签菜单 / 视图选择。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, row, space};
use iced::{Element, Length, Padding, border};
use atelyx_core::layout::{self, LayoutNode};

use crate::layout_view::{available_view_kinds, panel_tab_ids_in, view_label, view_icon, PopupState, RenameTarget};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::{App, Message};

pub(super) fn entries<'a>(app: &'a App, state: &PopupState) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::SceneSelector { confirm } => scene_selector_entries(app, confirm),
        PopupState::LayoutTabMenu { layout_id } => {
            let deletable = app
                .active_scene()
                .map(|s| s.layouts.len() > 1)
                .unwrap_or(false);
            let id = layout_id.clone();
            let mut entries = vec![(
                0usize,
                MenuEntry::action("重命名", Message::LayoutRenameStart(id.clone())),
            )];
            if deletable {
                entries.push((0, MenuEntry::danger("删除", "bin", Message::LayoutDelete(id))));
            }
            entries
        }
        PopupState::PanelMenu { panel_id } => panel_menu_entries(app, panel_id),
        PopupState::TabContext { panel_id, tab_id } => {
            tab_context_entries(app, panel_id, tab_id)
        }
        PopupState::ViewPicker { panel_id } => view_picker_entries(app, panel_id),
        PopupState::DetachedPanelMenu { window_id } => {
            let locked = app
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == *window_id)
                .and_then(|w| w.active_tab_id.clone())
                .map(|tid| {
                    app.ui
                        .detached_windows
                        .iter()
                        .find(|w| w.id == *window_id)
                        .and_then(|w| w.tabs.iter().find(|t| t.id == tid))
                        .is_some_and(|t| t.locked)
                })
                .unwrap_or(false);
            let wid = window_id.clone();
            vec![
                (
                    0,
                    MenuEntry::action(if locked { "解锁" } else { "锁定" },
                        Message::DetachedTabLock(wid.clone())),
                ),
                (0, MenuEntry::action("拖回主窗", Message::DetachedDockBack(wid))),
            ]
        }
        PopupState::DetachedTabContext { window_id, tab_id } => {
            let locked = app
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == *window_id)
                .and_then(|w| w.tabs.iter().find(|t| t.id == *tab_id))
                .is_some_and(|t| t.locked);
            let (wid, tid) = (window_id.clone(), tab_id.clone());
            let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
                (
                    0,
                    MenuEntry::Action {
                        label: "关闭该标签".into(),
                        icon: Some("x".into()),
                        danger: false,
                        disabled: locked,
                        on_press: (!locked).then(|| Message::DetachedTabClose(wid.clone(), tid.clone())),
                        note: None,
                        hint: locked.then(|| "已锁定，需先解锁".to_string()),
                    },
                ),
                (
                    0,
                    MenuEntry::Submenu {
                        label: "切换标签视图".into(),
                        icon: None,
                        disabled: locked,
                        pane: 1,
                    },
                ),
            ];
            // 视图清单：当前视图停用（角标「当前」）；他处占用停用（角标「已占用」）
            let current_view = app
                .ui
                .detached_windows
                .iter()
                .find(|w| w.id == *window_id)
                .and_then(|w| w.tabs.iter().find(|t| t.id == *tab_id))
                .map(|t| t.view.clone());
            for kind in available_view_kinds(app) {
                let entry = if current_view.as_deref() == Some(kind.as_str()) {
                    MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "当前", true, None)
                } else if view_occupied(app, &kind, None) {
                    MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "已占用", true, None)
                        .hinted("该视图已在其他位置（全局唯一）")
                } else {
                    MenuEntry::action_icon(
                        view_label(app, &kind),
                        view_icon(app, &kind),
                        Message::DetachedSetTabView {
                            window_id: wid.clone(),
                            tab_id: tid.clone(),
                            view: kind,
                        },
                    )
                };
                entries.push((1, entry));
            }
            entries
        }
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::SceneSelector { .. } => 208.0,
        PopupState::LayoutTabMenu { .. } => 144.0,
        PopupState::PanelMenu { .. } => 160.0,
        PopupState::TabContext { .. } | PopupState::DetachedTabContext { .. } => 176.0,
        PopupState::ViewPicker { .. } => 160.0,
        PopupState::DetachedPanelMenu { .. } => 160.0,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}

/// 视图 kind 是否已被占用（主树全部标签 + 撕裂窗口全部标签；`except_tab` 排除自身）。
fn view_occupied(app: &App, kind: &str, except_tab: Option<&str>) -> bool {
    fn tree_has(node: &LayoutNode, kind: &str, except: Option<&str>) -> bool {
        match node {
            LayoutNode::Panel { tabs, .. } => tabs
                .iter()
                .any(|t| t.view == kind && except != Some(t.id.as_str())),
            LayoutNode::Split { children, .. } => {
                children.iter().any(|c| tree_has(c, kind, except))
            }
        }
    }
    let main_tree = layout::active_layout(&app.ui).tree;
    let main_hit = tree_has(&main_tree, kind, except_tab);
    let detached_hit = app.ui.detached_windows.iter().any(|w| {
        w.tabs
            .iter()
            .any(|t| t.view == kind && except_tab != Some(t.id.as_str()))
    });
    main_hit || detached_hit
}

/// 场景选择器：根面板（场景行 + 新建）与删除确认面板。
fn scene_selector_entries(app: &App, confirm: &Option<String>) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let mut root: Vec<(usize, MenuEntry<'static, Message>)> = vec![(0, MenuEntry::Group("场景".into()))];
    let active_id = app.ui.active_scene_id.clone();
    for s in &app.ui.scenes {
        let is_active = active_id.as_deref() == Some(s.id.as_str());
        let id = s.id.clone();
        let name = s.name.clone();
        let is_default = s.id == layout::DEFAULT_SCENE_ID;
        // 场景行 = 激活按钮 + 行尾动作（默认场景无动作；重命名在途 = 行内输入）
        let renaming = matches!(
            &app.rename,
            Some(r) if r.target == RenameTarget::Scene(id.clone())
        );
        let name_area: Element<'static, Message> = if renaming {
            controls::inline_input(
                iced::widget::Id::new(controls::INLINE_RENAME_INPUT),
                "新名称",
                &app.rename.as_ref().map(|r| r.value.clone()).unwrap_or_default(),
                96.0,
                Message::SceneRenameInput,
                Message::LayoutRenameCommit,
                Message::RenamePressInside,
            )
        } else {
            button(
                row![
                    text(name.clone())
                        .size(pal::fs_caption())
                        .color(if is_active { pal::ACCENT } else { pal::TEXT_PRIMARY }),
                ]
                .push(if is_active {
                    icon("check", 12.0, pal::ACCENT)
                } else {
                    space().width(0.0).into()
                })
                .spacing(4),
            )
            .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
            .width(Length::Fill)
            .style(|_t, status| button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then_some(pal::HOVER.into()),
                ..Default::default()
            })
            .on_press(Message::SceneActivate(id.clone()))
            .into()
        };
        let mut row = row![name_area].spacing(2);
        if !is_default {
            // 行尾动作钮：圆角 + 悬停底（显形时机 = 常显）
            row = row.push(
                button(icon("pencil", 11.0, pal::TEXT_MUTED))
                    .padding(2.0)
                    .style(|_t, status| button::Style {
                        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                            .then_some(pal::HOVER.into()),
                        border: border::rounded(pal::RADIUS_XS),
                        ..Default::default()
                    })
                    .on_press(Message::SceneRenameStart(id.clone())),
            );
            row = row.push(
                button(icon("bin", 11.0, pal::DANGER))
                    .padding(2.0)
                    .style(|_t, status| button::Style {
                        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                            .then_some(pal::HOVER.into()),
                        border: border::rounded(pal::RADIUS_XS),
                        ..Default::default()
                    })
                    .on_press(Message::SceneDeleteAsk(id.clone())),
            );
        }
        root.push((0, MenuEntry::Custom(
            container(row).padding(Padding::ZERO.horizontal(4.0)).width(Length::Fill).into(),
            34.0,
        )));
    }
    root.push((0, MenuEntry::action_icon("新建场景", "plus", Message::SceneNew)));

    // 确认面板：标题用强调字重，按钮区与文案间距 12
    let confirm_entries: Vec<(usize, MenuEntry<'static, Message>)> = if let Some(id) = confirm {
        let scene = app.ui.scenes.iter().find(|s| s.id == *id);
        let name = scene.map(|s| s.name.clone()).unwrap_or_default();
        let count = scene.map(|s| s.layouts.len()).unwrap_or(0);
        let pane = column![
            text(format!("删除场景「{name}」？"))
                .size(pal::fs_caption())
                .color(pal::TEXT_PRIMARY)
                .font(crate::ui::bold()),
            text(format!("其下 {count} 套布局将一并删除。"))
                .size(pal::fs_caption())
                .color(pal::TEXT_MUTED),
            row![
                container(button(text("取消"))
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::TEXT_SECONDARY,
                        border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::Ui(crate::ui::UiMsg::PopupPane(0))))
                .align_x(Horizontal::Right),
                button(text("删除"))
                    .padding(Padding::ZERO.horizontal(10.0).vertical(4.0))
                    .style(|_t, _s| button::Style {
                        text_color: pal::DANGER,
                        border: border::rounded(pal::RADIUS_SM).color(pal::DANGER).width(1.0),
                        ..Default::default()
                    })
                    .on_press(Message::SceneDelete(id.clone())),
            ]
            .spacing(8)
            .padding(Padding::ZERO.top(8.0))
            .align_y(Vertical::Center),
        ]
        .spacing(4)
        .padding(12.0)
        .width(Length::Fill);
        vec![(1, MenuEntry::Custom(pane.into(), 114.0))]
    } else {
        Vec::new()
    };
    root.into_iter().chain(confirm_entries).collect()
}

/// 面板 ≡ 菜单：锁定（整块）/ 删除面板（最后一个禁用）/ 分割；空面板无锁定项。
fn panel_menu_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let has_active = find_active_tab_in(app, panel_id).is_some();
    let main_tree = layout::active_layout(&app.ui).tree;
    let panel_count = count_panels(&main_tree);
    // 整块锁定基准 = 面板全部标签已锁
    let locked = has_active
        && panel_tab_ids_in(&layout::active_layout(&app.ui).tree, panel_id)
            .iter()
            .all(|id| {
                layout::find_tab_in_tree(&main_tree, id).is_some_and(|(_, t)| t.locked)
            });
    let pid = panel_id.to_string();
    let mut entries = Vec::new();
    if has_active {
        entries.push((
            0,
            MenuEntry::action_icon(
                if locked { "解锁" } else { "锁定" },
                if locked { "lock-open" } else { "lock" },
                Message::PanelTabLock(pid.clone()),
            ),
        ));
    }
    if !locked {
        entries.push((
            0,
            MenuEntry::Action {
                label: "删除面板".into(),
                icon: None,
                danger: true,
                disabled: panel_count <= 1,
                on_press: (panel_count > 1).then(|| Message::PanelClose(pid.clone())),
                note: None,
                hint: (panel_count <= 1).then(|| "最后一个面板不可删除".to_string()),
            },
        ));
        entries.push((0, MenuEntry::Divider));
        entries.push((0, MenuEntry::action("左右分割", Message::PanelSplit(pid.clone(), "horizontal".into()))));
        entries.push((0, MenuEntry::action("上下分割", Message::PanelSplit(pid, "vertical".into()))));
    }
    entries
}

/// 主树活动标签信息（(tab_id, locked)）。
fn find_active_tab_in(app: &App, panel_id: &str) -> Option<(String, bool)> {
    let tree = layout::active_layout(&app.ui).tree;
    let LayoutNode::Panel { tabs, active_tab_id, .. } = layout::find_node(&tree, panel_id)? else {
        return None;
    };
    let active_id = active_tab_id.as_deref()?;
    tabs.iter().find(|t| t.id == active_id).map(|t| (t.id.clone(), t.locked))
}

fn count_panels(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Panel { .. } => 1,
        LayoutNode::Split { children, .. } => children.iter().map(count_panels).sum(),
    }
}

/// 标签右键菜单：根面板（关闭 / 切换视图）与视图清单面板。
fn tab_context_entries(
    app: &App,
    panel_id: &str,
    tab_id: &str,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let tab = find_tab_in_active(app, tab_id);
    let locked = tab.as_ref().is_some_and(|(_, locked)| *locked);
    let current_view = tab.as_ref().map(|(view, _)| view.clone());
    let (pid, tid) = (panel_id.to_string(), tab_id.to_string());
    let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![
        (
            0,
            MenuEntry::Action {
                label: "关闭该标签".into(),
                icon: Some("x".into()),
                danger: false,
                disabled: locked,
                on_press: (!locked).then(|| Message::PanelTabClose(pid.clone(), tid.clone())),
                note: None,
                hint: locked.then(|| "已锁定，需先解锁".to_string()),
            },
        ),
        (
            0,
            MenuEntry::Submenu {
                label: "切换标签视图".into(),
                icon: None,
                disabled: locked,
                pane: 1,
            },
        ),
    ];
    for kind in available_view_kinds(app) {
        let entry = if current_view.as_deref() == Some(kind.as_str()) {
            MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "当前", true, None)
        } else if view_occupied(app, &kind, Some(tab_id)) {
            MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "已占用", true, None)
                .hinted("该视图已在其他位置（全局唯一）")
        } else {
            MenuEntry::action_icon(
                view_label(app, &kind),
                view_icon(app, &kind),
                Message::SetTabView { panel_id: pid.clone(), tab_id: tid.clone(), view: kind },
            )
        };
        entries.push((1, entry));
    }
    entries
}

/// 主树标签信息（(view, locked)）。
fn find_tab_in_active(app: &App, tab_id: &str) -> Option<(String, bool)> {
    let tree = layout::active_layout(&app.ui).tree;
    layout::find_tab_in_tree(&tree, tab_id).map(|(_, t)| (t.view, t.locked))
}

/// 空面板右键的视图选择器：本组 = 点击激活已有标签（角标「本组」）；已占用停用；其余添加。
fn view_picker_entries(app: &App, panel_id: &str) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let pid = panel_id.to_string();
    available_view_kinds(app)
        .into_iter()
        .map(|kind| {
            let entry = if let Some(tab_id) = panel_tab_with_view(app, panel_id, &kind) {
                MenuEntry::noted(
                    view_label(app, &kind),
                    view_icon(app, &kind),
                    "本组",
                    false,
                    Some(Message::PanelTab(pid.clone(), tab_id)),
                )
            } else if view_occupied(app, &kind, None) {
                MenuEntry::noted(view_label(app, &kind), view_icon(app, &kind), "已占用", true, None)
                    .hinted("该视图已在其他位置（全局唯一）")
            } else {
                MenuEntry::action_icon(
                    view_label(app, &kind),
                    view_icon(app, &kind),
                    Message::PanelAddView { panel_id: pid.clone(), view: kind },
                )
            };
            (0, entry)
        })
        .collect()
}

/// 面板内持有该视图的标签 id。
fn panel_tab_with_view(app: &App, panel_id: &str, kind: &str) -> Option<String> {
    let tree = layout::active_layout(&app.ui).tree;
    layout::find_panel(&tree, panel_id)?.iter().find(|t| t.view == kind).map(|t| t.id.clone())
}
