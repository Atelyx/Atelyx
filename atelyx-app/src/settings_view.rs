//! 设置整页：左栏分组导航（应用级 / 仓库级 / 关于）+ 右侧分区内容（本模块或 `settings_view/` 子模块）。
//! 分区只渲染表单，配置真源在 `App.global`（应用级）与仓库 `VaultConfig`（仓库级）。

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Padding, border, widget::container::Style};

use crate::layout_view::{format_ts, mono, parse_hex_color};
use crate::ui::controls;
use crate::ui::icon::icon;
use crate::ui::palette as pal;
use crate::ui::semibold;
use crate::{App, Message};

mod about;
pub(crate) mod ai;
pub(crate) mod editor;
mod editor_prefs;
mod files;
mod general;
mod shortcuts;
mod workspace;
// 关于页许可模态由 layout_view 的覆盖层分派挂载
pub(crate) use about::{LICENSE_CARD_WIDTH, license_overlay};

/// 当前仓库容器（本地仓库或协作空间）的展示名；未打开 = None。
pub(crate) fn repo_display_name(app: &App) -> Option<String> {
    if let Some(root) = &app.vault_root {
        return Some(atelyx_core::vault::vault_display_name(root));
    }
    crate::space_actions::session_of(app).map(|s| s.name)
}

struct SettingsItem {
    id: &'static str,
    label: &'static str,
    icon: &'static str,
}

const APP_SETTINGS: &[SettingsItem] = &[
    SettingsItem { id: "general", label: "通用", icon: "settings" },
    SettingsItem { id: "collab", label: "多人协作", icon: "users" },
    SettingsItem { id: "editorPrefs", label: "编辑器", icon: "pen-line" },
    SettingsItem { id: "shortcuts", label: "快捷键", icon: "keyboard" },
    SettingsItem { id: "plugins", label: "插件", icon: "puzzle" },
];

const REPO_SETTINGS: &[SettingsItem] = &[
    SettingsItem { id: "ai", label: "AI 设置", icon: "bot" },
    SettingsItem { id: "files", label: "文件与路径", icon: "folder-tree" },
    SettingsItem { id: "workspace", label: "工作区", icon: "panels-top-left" },
    SettingsItem { id: "editor", label: "编辑器", icon: "pen-line" },
];

fn settings_item(item: &SettingsItem, active_tab: &str) -> Element<'static, Message> {
    let active = item.id == active_tab;
    button(
        row![
            icon(item.icon, 14.0, if active { pal::ACCENT } else { pal::TEXT_SECONDARY }),
            text(item.label)
                .size(pal::fs_caption())
                .color(if active { pal::ACCENT } else { pal::TEXT_SECONDARY })
                .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
        ]
        .spacing(8),
    )
    .padding(Padding::ZERO.horizontal(10.0).vertical(6.0))
    .width(Length::Fill)
    .style(move |_t, status| button::Style {
        background: if active {
            Some(pal::ACCENT_SOFT.into())
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(pal::HOVER.into())
        } else {
            None
        },
        text_color: pal::TEXT_PRIMARY,
        border: border::rounded(pal::RADIUS_SM),
        ..Default::default()
    })
    .on_press(Message::SettingsTab(item.id.to_string()))
    .into()
}

/// 设置整页：左栏分组导航（应用级 / 仓库级 / 关于，可折叠）+ 右侧分区内容。
pub fn settings_view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let collapsed = app.settings_collapsed;
    // 左栏宽度 = 内容区 20%，钳制 180–280；折叠态固定 44
    let nav_w = if collapsed {
        44.0
    } else {
        (app.main_size.width * 0.2).clamp(180.0, 280.0)
    };
    let active = app.settings_tab.as_str();
    let mut nav = column![].spacing(4);
    nav = push_group(nav, "应用级", "全局共享", false, APP_SETTINGS, active);
    // 仓库级分组：本地仓库与协作空间（配置真源分别在本地 .atelyx/ 与服务端）都可达
    if let Some(repo_name) = repo_display_name(app) {
        nav = nav.push(container(controls::h_divider_subtle()).padding(Padding::ZERO.top(8.0).bottom(12.0)));
        nav = push_group(nav, "仓库级", &repo_name, true, REPO_SETTINGS, active);
    }

    let header: Element<'static, Message> = if collapsed {
        container(
            button(icon("arrow-left", 15.0, pal::TEXT_SECONDARY))
                .padding(6.0)
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    border: border::rounded(pal::RADIUS_SM),
                    ..Default::default()
                })
                .on_press(Message::SettingsToggle),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding(Padding::ZERO.vertical(12.0))
        .into()
    } else {
        row![
            text("设置").size(pal::fs_body()).color(pal::TEXT_PRIMARY).font(semibold()),
            space().width(Length::Fill),
            button(icon("x", 14.0, pal::TEXT_SECONDARY))
                .padding(4.0)
                .style(|_t, status| button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then_some(pal::HOVER.into()),
                    ..Default::default()
                })
                .on_press(Message::SettingsToggle),
        ]
        .spacing(8)
        .padding(Padding::ZERO.horizontal(12.0).vertical(12.0))
        .into()
    };

    // 左栏：标题行 + 滚动导航 + 底部「关于」（不随分组滚走）+ 折叠钮
    let mut aside_col = column![header];
    if !collapsed {
        aside_col = aside_col.push(scrollable(container(nav).padding(8.0)).width(Length::Fill).height(Length::Fill));
        aside_col = aside_col.push(controls::h_divider_subtle());
        aside_col = aside_col.push(
            container(settings_item(&SettingsItem { id: "about", label: "关于", icon: "info" }, active))
                .width(Length::Fill)
                .padding(Padding::ZERO.horizontal(8.0).top(8.0)),
        );
    }
    let collapse_label: Element<'static, Message> = if collapsed {
        space().width(Length::Fixed(0.0)).into()
    } else {
        text("折叠".to_string()).size(pal::fs_caption()).color(pal::TEXT_SECONDARY).into()
    };
    let collapse_btn = button(
        row![
            icon(if collapsed { "chevron-right" } else { "chevron-left" }, 14.0, pal::TEXT_SECONDARY),
            collapse_label,
        ]
        .spacing(4),
    )
    .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
    .style(|_t, status| button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(pal::HOVER.into()),
        border: border::rounded(pal::RADIUS_SM),
        ..Default::default()
    })
    .on_press(Message::SettingsNavCollapse);
    aside_col = aside_col.push(container(collapse_btn).padding(8.0).width(Length::Fill));

    let aside = container(aside_col.width(Length::Fill).height(Length::Fill))
        .width(Length::Fixed(nav_w))
        .height(Length::Fill)
        .style(|_t| Style {
            background: Some(pal::BG_PRIMARY.into()),
            ..Style::default()
        });

    // 页头：标题 + 作用域徽标 + 描述（含仓库名）
    let (title, scope_vault, desc) = tab_meta(active, app);
    let scope_pill = controls::badge(
        if scope_vault { "仓库级" } else { "应用级" },
        if scope_vault { pal::ACCENT_SOFT } else { pal::BG_TERTIARY },
        if scope_vault { pal::ACCENT } else { pal::TEXT_PRIMARY },
    );
    let body = column![
        container(
            column![
                row![
                    text(title.to_string()).size(pal::fs_h2()).color(pal::TEXT_PRIMARY).font(semibold()),
                    space().width(Length::Fill),
                    scope_pill,
                ],
                text(desc).size(pal::fs_caption()).color(pal::TEXT_MUTED),
            ]
            .spacing(2),
        )
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(20.0).vertical(12.0)),
        controls::h_divider_subtle(),
        settings_body(app, window),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    row![aside, vline(), container(body).width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 分隔竖线（左栏与内容区之间）。
fn vline() -> Element<'static, Message> {
    container(space())
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_t| Style {
            background: Some(pal::BORDER_SUBTLE.into()),
            ..Style::default()
        })
        .into()
}

/// 分区内容分派（未实现分区 = 占位空态）。
fn settings_body(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    match app.settings_tab.as_str() {
        "general" => general::view(app),
        "collab" => collab_settings_view(app),
        "editorPrefs" => editor_prefs::view(app),
        "shortcuts" => shortcuts::view(app, window),
        "ai" => ai::view(app, window),
        "files" => files::view(app),
        "workspace" => workspace::view(app, window),
        "editor" => editor::view(app),
        "about" => about::view(app, window),
        _ => controls::empty_state("puzzle", "暂无设置项", "该分组暂无可配置项", None),
    }
}

/// 设置行卡片：左标题与描述，右控件（垂直居中）。
pub fn setting_row(
    title: &str,
    desc: &str,
    control: Element<'static, Message>,
) -> Element<'static, Message> {
    card(
        row![
            column![
                text(title.to_string())
                    .size(pal::fs_body())
                    .color(pal::TEXT_PRIMARY)
                    .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
                text(desc.to_string()).size(pal::fs_caption()).color(pal::TEXT_MUTED),
            ]
            .spacing(2)
            .width(Length::Fill),
            control,
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center),
    )
}

/// 设置页卡片容器（条目行 / 信息行的统一体块形态）。
pub(crate) fn card(content: iced::widget::Row<'static, Message>) -> Element<'static, Message> {
    container(content)
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(12.0).vertical(10.0))
        .style(|_t| Style {
            background: Some(pal::BG_TERTIARY.into()),
            border: border::rounded(pal::RADIUS_MD),
            ..Style::default()
        })
        .into()
}

/// 一组导航：分组头 + 条目（条目间距 4）。
fn push_group<'a>(
    nav: iced::widget::Column<'a, Message>,
    label: &str,
    hint: &str,
    repo_level: bool,
    items: &'static [SettingsItem],
    active_tab: &str,
) -> iced::widget::Column<'a, Message> {
    let mut col = column![group_head(label, hint, repo_level)].spacing(4);
    for item in items {
        col = col.push(settings_item(item, active_tab));
    }
    nav.push(col)
}

/// 设置页「多人协作」：登录身份、协作展示色、设备会话管理、退出登录。
fn collab_settings_view(app: &App) -> Element<'static, Message> {
    let mut col = iced::widget::Column::<Message>::new()
        .spacing(8)
        .padding(Padding::ZERO.horizontal(20.0));
    match &app.global.last_identity {
        None => {
            return container(controls::empty_state(
                "users",
                "未登录",
                "登录后管理账号与设备会话",
                None,
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }
        Some(identity) => {
            col = col.push(group_head("登录身份", &identity.server_url, false));
            col = col.push(card(
                row![
                    icon("user", 14.0, pal::ACCENT),
                    text(identity.display_name.clone())
                        .size(pal::fs_body())
                        .color(pal::TEXT_PRIMARY)
                        .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
                    text(format!("@ {}", identity.server_url))
                        .size(pal::fs_caption())
                        .color(pal::TEXT_MUTED),
                    space().width(Length::Fill),
                    controls::btn(
                        "退出登录",
                        &controls::btn_sm(),
                        controls::Variant::Ghost,
                        Some(Message::Space(crate::SpaceMsg::Logout)),
                    ),
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
            ));
            // 协作展示色（只读展示：编辑留待外观设置统一收口）
            col = col.push(group_head("协作展示色", "远端光标 / 名单", false));
            let color_swatch = app
                .global
                .collab_color
                .clone()
                .and_then(|hex| parse_hex_color(&hex))
                .unwrap_or(pal::TEXT_MUTED);
            col = col.push(card(
                row![
                    container(space())
                        .width(Length::Fixed(12.0))
                        .height(Length::Fixed(12.0))
                        .style(move |_t| Style {
                            background: Some(color_swatch.into()),
                            border: border::rounded(999.0),
                            ..Style::default()
                        }),
                    text(app.global.collab_color.clone().unwrap_or_else(|| "未指派".into()))
                        .size(pal::fs_caption())
                        .color(pal::TEXT_SECONDARY)
                        .font(mono()),
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
            ));
            // 设备会话
            col = col.push(group_head("设备会话", &format!("{} 台在线记录", app.space_devices.len()), false));
            for d in &app.space_devices {
                let badge: Element<'static, Message> = if d.current {
                    text("本机").size(pal::fs_micro()).color(pal::TEXT_MUTED).into()
                } else {
                    controls::btn(
                        "吊销",
                        &controls::btn_sm(),
                        controls::Variant::Ghost,
                        Some(Message::Space(crate::SpaceMsg::RevokeDevice(d.id.clone()))),
                    )
                    .into()
                };
                col = col.push(card(
                    row![
                        icon(
                            if d.current { "monitor-check" } else { "monitor" },
                            14.0,
                            if d.current { pal::ACCENT } else { pal::TEXT_MUTED },
                        ),
                        text(d.device_name.clone())
                            .size(pal::fs_body())
                            .color(pal::TEXT_PRIMARY)
                            .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
                        text(format!("最近活跃 {}", format_ts(d.last_seen_at * 1000)))
                            .size(pal::fs_caption())
                            .color(pal::TEXT_MUTED),
                        space().width(Length::Fill),
                        badge,
                    ]
                    .spacing(8)
                    .align_y(iced::alignment::Vertical::Center),
                ));
            }
            if app.space_devices.is_empty() {
                col = col.push(container(
                    text("暂无设备记录").size(pal::fs_micro()).color(pal::TEXT_MUTED),
                ));
            }
        }
    }
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 分组头：竖条 + 标题 + 右对齐 hint（hint 空串 = 不展示）。
pub fn group_head(label: &str, hint: &str, repo_level: bool) -> Element<'static, Message> {
    let bar_color = if repo_level { pal::ACCENT } else { pal::BORDER_STRONG };
    let text_color = if repo_level { pal::ACCENT } else { pal::TEXT_MUTED };
    let mut head = row![
        container(space())
            .width(Length::Fixed(3.0))
            .height(Length::Fixed(11.0))
            .style(move |_t| Style {
                background: Some(bar_color.into()),
                border: border::rounded(2.0),
                ..Style::default()
            }),
        text(label.to_string())
            .size(pal::fs_micro())
            .color(text_color)
            .font(semibold()),
    ]
    .spacing(8)
    .width(Length::Fill);
    if !hint.is_empty() {
        head = head
            .push(space().width(Length::Fill))
            .push(text(hint.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED).font(mono()));
    }
    container(head)
        .padding(Padding::ZERO.horizontal(8.0).bottom(8.0))
        .into()
}

/// 页面标题 / 作用域 / 描述（仓库级描述含仓库名）。
fn tab_meta(tab: &str, app: &App) -> (&'static str, bool, String) {
    let vault_name = repo_display_name(app).unwrap_or_default();
    let app_desc = "全局共享配置".to_string();
    let vault_desc = format!("作用于当前仓库（{vault_name}）");
    match tab {
        "general" => ("通用", false, app_desc),
        "collab" => ("多人协作", false, app_desc),
        "editorPrefs" => ("编辑器", false, app_desc),
        "shortcuts" => ("快捷键", false, app_desc),
        "plugins" => ("插件", false, app_desc),
        "ai" => ("AI 设置", true, vault_desc),
        "files" => ("文件与路径", true, vault_desc),
        "workspace" => ("工作区", true, vault_desc),
        "editor" => ("编辑器", true, vault_desc),
        "about" => ("关于", false, "版本 · 检查更新 · 项目主页".to_string()),
        _ => ("设置", false, String::new()),
    }
}
