//! 协作空间弹层：身份菜单 / 登录注册 / 创建与加入 / 成员与邀请码。

use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, space};
use iced::{Element, Length, Padding, border};

use crate::layout_view::{role_badge, PopupOpen, PopupState};
use crate::ui::controls::{self, Variant};
use crate::ui::icon::icon;
use crate::ui::menu::MenuEntry;
use crate::ui::palette as pal;
use crate::ui::{mono, text};
use crate::{App, Message};

pub(super) fn entries<'a>(app: &'a App, state: &PopupState, window: iced::window::Id) -> Vec<(usize, MenuEntry<'a, Message>)> {
    match state {
        PopupState::SpaceMenu => space_menu_entries(app, window),
        PopupState::SpaceLogin { register } => space_login_entries(app, *register),
        PopupState::SpaceCreate => vec![
            (
                0,
                MenuEntry::Custom(
                    space_form_input("空间名", &app.space_form.name.clone(), |v| {
                        Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::Name, v))
                    }, false),
                    30.0,
                ),
            ),
            (
                0,
                MenuEntry::Custom(space_hint("创建后你将是该空间的拥有者（owner）"), 18.0),
            ),
            (
                0,
                MenuEntry::Custom(space_action_row(
                    "创建空间",
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::CreateSpace),
                ), 30.0),
            ),
        ],
        PopupState::SpaceJoin => vec![
            (
                0,
                MenuEntry::Custom(
                    space_form_input("邀请码", &app.space_form.code.clone(), |v| {
                        Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::Code, v))
                    }, false),
                    30.0,
                ),
            ),
            (
                0,
                MenuEntry::Custom(space_action_row(
                    "加入空间",
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::JoinSpace),
                ), 30.0),
            ),
        ],
        PopupState::SpaceRowMenu { server_url, space_id } => {
            let active = matches!(
                app.repo_kind,
                crate::RepoKind::Space(ref s)
                    if s.server_url == *server_url && s.space_id == *space_id
            );
            // 权限口径对齐服务端：名册对全部成员可见（进空间后可看），
            // 邀请码（列出/生成/撤销）与管理操作为 owner 专属
            let owner = active
                && matches!(&app.repo_kind, crate::RepoKind::Space(s) if s.role == atelyx_core::space_client::SpaceRole::Owner);
            let mut entries: Vec<(usize, MenuEntry<'static, Message>)> = vec![(
                0,
                MenuEntry::Action {
                    label: "成员管理".into(),
                    icon: Some("users".into()),
                    danger: false,
                    disabled: !active,
                    on_press: active.then(|| {
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::SpaceMembers,
                                min_width: 0.0,
                            },
                        )
                    }),
                    note: None,
                    hint: (!active).then(|| "进入空间后可查看成员".to_string()),
                },
            )];
            entries.push((
                0,
                MenuEntry::Action {
                    label: "邀请码".into(),
                    icon: Some("user-plus".into()),
                    danger: false,
                    disabled: !owner,
                    on_press: owner.then(|| {
                        Message::PopupOpen(
                            window,
                            PopupOpen {
                                point: app.cursor_for(window),
                                flip_y: None,
                                state: PopupState::SpaceInvite,
                                min_width: 0.0,
                            },
                        )
                    }),
                    note: None,
                    hint: Some(if active {
                        "邀请码仅空间所有者可管理".to_string()
                    } else {
                        "进入空间后可管理".to_string()
                    }),
                },
            ));
            entries.push((0, MenuEntry::Divider));
            entries.push((
                0,
                MenuEntry::Action {
                    label: "从列表移除".into(),
                    icon: Some("x".into()),
                    danger: true,
                    disabled: active,
                    on_press: (!active).then(|| Message::RemoveSpaceAsk {
                        server_url: server_url.clone(),
                        space_id: space_id.clone(),
                    }),
                    note: None,
                    hint: active.then(|| "空间已激活，先进入其他仓库或空间".to_string()),
                },
            ));
            entries
        }
        PopupState::SpaceMembers => space_members_entries(app),
        PopupState::SpaceInvite => space_invite_entries(app),
        _ => unreachable!("按域分派，本域以外的变体不会到这里"),
    }
}

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        PopupState::SpaceMenu => 200.0,
        PopupState::SpaceLogin { .. } => 340.0,
        PopupState::SpaceCreate => 260.0,
        PopupState::SpaceJoin => 260.0,
        PopupState::SpaceRowMenu { .. } => 176.0,
        // 成员行带 @username 与角色徽标；邀请码表单含两个数字输入
        PopupState::SpaceMembers => 320.0,
        PopupState::SpaceInvite => 320.0,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}

/// 打开新弹层（同窗口二段导航：菜单 → 表单）。
fn space_sub_popup(window: iced::window::Id, app: &App, state: PopupState) -> Message {
    Message::PopupOpen(
        window,
        PopupOpen {
            point: app.cursor_for(window),
            flip_y: None,
            state,
            min_width: 0.0,
        },
    )
}

/// 协作空间菜单：身份概览 / 登录（切换账号）/ 创建 / 邀请码加入。
/// 账号下的空间直接列在文件面板空间区（登录即见），不在弹层中转。
fn space_menu_entries(app: &App, window: iced::window::Id) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
    if let Some(identity) = &app.global.last_identity {
        entries.push((
            0,
            MenuEntry::Custom(
                space_hint(&format!("{} @ {}", identity.display_name, identity.server_url)),
                18.0,
            ),
        ));
    }
    entries.push((
        0,
        MenuEntry::action_icon(
            if app.global.last_identity.is_some() { "登录其他账号" } else { "登录协作服务器" },
            "log-in",
            space_sub_popup(window, app, PopupState::SpaceLogin { register: false }),
        ),
    ));
    if app.global.last_identity.is_some() {
        entries.push((0, MenuEntry::action_icon("创建空间", "plus", space_sub_popup(window, app, PopupState::SpaceCreate))));
        entries.push((0, MenuEntry::action_icon("邀请码加入", "user-plus", space_sub_popup(window, app, PopupState::SpaceJoin))));
    }
    entries
}

/// 协作登录 / 注册表单（地址 + 账号 + 密码 + 设备名）。
fn space_login_entries(
    app: &App,
    register: bool,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    let form = &app.space_form;
    let field = |f: crate::SpaceField| move |v: String| Message::Space(crate::SpaceMsg::LoginField(f, v));
    let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
        (
            0,
            MenuEntry::Custom(
                space_form_input("服务器地址（如 192.168.1.5:11224）", &form.server.clone(), field(crate::SpaceField::Server), false),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input("用户名", &form.username.clone(), field(crate::SpaceField::Username), false),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input("密码", &form.password.clone(), field(crate::SpaceField::Password), true),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input("设备名（可选）", &form.device.clone(), field(crate::SpaceField::Device), false),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_action_row(
                    if register { "注册并登录" } else { "登录" },
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::LoginSubmit { register }),
                ),
                30.0,
            ),
        ),
    ];
    let mode = if register {
        ("已有账号？去登录", false)
    } else {
        ("没有账号？去注册", true)
    };
    entries.push((0, MenuEntry::action(mode.0, Message::SpaceLoginToggle(mode.1))));
    entries.push((0, MenuEntry::Custom(space_hint("令牌保存在系统密钥链，不落明文"), 18.0)));
    entries
}

/// 弹层表单输入行（占位文案 + 字段映射；secure = 掩码显示，用于密码）。
fn space_form_input<'a>(
    placeholder: &str,
    draft: &str,
    on_input: impl Fn(String) -> Message + 'a,
    secure: bool,
) -> Element<'a, Message> {
    let input = iced::widget::text_input(placeholder, draft)
        .size(pal::fs_caption())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .on_input(on_input)
        .style(|_theme, status| controls::form_input_style(status));
    if secure { input.secure(true).into() } else { input.into() }
}

/// 弹层表单的说明行。
fn space_hint(text_content: &str) -> Element<'static, Message> {
    container(text(text_content.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED))
        .padding(Padding::ZERO.horizontal(12.0))
        .width(Length::Fill)
        .into()
}

/// 弹层表单的提交行（按钮全宽；在途禁用）。
fn space_action_row(
    label: &str,
    busy: bool,
    on_press: Message,
) -> Element<'static, Message> {
    container(
        controls::btn(
            label,
            &controls::btn_sm(),
            controls::Variant::Secondary,
            (!busy).then_some(on_press),
        )
        .width(Length::Fill),
    )
    .padding(Padding::ZERO.horizontal(12.0))
    .width(Length::Fill)
    .into()
}

/// 弹层红字错误条（数据加载失败的非静默呈现）。
fn popup_error_bar(message: &str) -> Element<'static, Message> {
    container(
        row![
            icon("info", 12.0, pal::DANGER),
            text(message.to_string()).size(pal::fs_micro()).color(pal::DANGER),
        ]
        .spacing(6),
    )
    .padding(Padding::ZERO.horizontal(12.0).vertical(3.0))
    .width(Length::Fill)
    .into()
}

/// 成员管理弹层：名册行（显示名 + @username + 角色徽标）+ owner 的行内二段确认 + 错误条。
/// 管理操作仅 owner 可见（服务端同口径）；owner 行无操作。
fn space_members_entries(
    app: &App,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    use atelyx_core::space_client::SpaceRole;
    let mut entries: Vec<(usize, MenuEntry<Message>)> = Vec::new();
    if let Some(err) = &app.space_members_error {
        entries.push((0, MenuEntry::Custom(popup_error_bar(err), 22.0)));
    } else if app.space_members.is_none() {
        entries.push((0, MenuEntry::Custom(space_hint(if app.space_members_loading { "加载中…" } else { "暂无名册" }), 18.0)));
    }
    let owner = matches!(
        &app.repo_kind,
        crate::RepoKind::Space(s) if s.role == SpaceRole::Owner
    );
    for m in app.space_members.iter().flatten() {
        // 行内二段确认态：整行替换为问题 + 确认/取消
        if let Some(op) = app
            .space_member_confirm
            .as_ref()
            .filter(|(uid, _)| uid == &m.user_id)
            .map(|(_, op)| *op)
        {
            let (question, ok_label, variant, ok_msg) = match op {
                crate::space_actions::MemberConfirm::Transfer => (
                    "转让管理权给该成员？",
                    "确认转让",
                    Variant::Secondary,
                    Message::Space(crate::SpaceMsg::MemberTransfer(m.user_id.clone())),
                ),
                crate::space_actions::MemberConfirm::Remove => (
                    "将该成员移出空间？",
                    "确认移除",
                    Variant::Danger,
                    Message::Space(crate::SpaceMsg::MemberRemove(m.user_id.clone())),
                ),
            };
            entries.push((
                0,
                MenuEntry::Custom(
                    container(
                        row![
                            text(question).size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
                            space().width(Length::Fill),
                            controls::btn(ok_label, &controls::btn_sm(), variant, Some(ok_msg)),
                            controls::btn(
                                "取消",
                                &controls::btn_sm(),
                                Variant::Ghost,
                                Some(Message::Space(crate::SpaceMsg::MemberAskCancel)),
                            ),
                        ]
                        .spacing(6),
                    )
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill)
                    .height(Length::Fixed(pal::MENU_ITEM_H))
                    .align_y(Vertical::Center)
                    .into(),
                    pal::MENU_ITEM_H,
                ),
            ));
            continue;
        }
        let mut row = row![
            text(m.display_name.clone()).size(pal::fs_caption()).color(pal::TEXT_PRIMARY),
            text(format!("@{}", m.username)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
            role_badge(m.role),
            space().width(Length::Fill),
        ]
        .spacing(6);
        if owner && m.role != SpaceRole::Owner {
            let uid = m.user_id.clone();
            row = row
                .push(controls::btn(
                    "转让",
                    &controls::btn_sm(),
                    Variant::Secondary,
                    Some(Message::Space(crate::SpaceMsg::MemberAsk(
                        uid.clone(),
                        crate::space_actions::MemberConfirm::Transfer,
                    ))),
                ))
                .push(controls::btn(
                    "移除",
                    &controls::btn_sm(),
                    Variant::Danger,
                    Some(Message::Space(crate::SpaceMsg::MemberAsk(
                        uid,
                        crate::space_actions::MemberConfirm::Remove,
                    ))),
                ));
        }
        entries.push((
            0,
            MenuEntry::Custom(
                container(row)
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill)
                    .height(Length::Fixed(pal::MENU_ITEM_H))
                    .align_y(Vertical::Center)
                    .into(),
                pal::MENU_ITEM_H,
            ),
        ));
    }
    entries
}

/// 邀请码元信息行：{角色} · 有效期 · 次数。
fn invite_meta_line(info: &atelyx_core::space_client::InviteInfo) -> String {
    use atelyx_core::space_client::SpaceRole;
    let role = match info.role {
        SpaceRole::Owner => "所有者",
        SpaceRole::Editor => "编辑者",
        SpaceRole::Viewer => "只读",
    };
    let expires = match info.expires_at {
        None => "永久有效".to_string(),
        Some(secs) => format!("过期于 {}", atelyx_core::date::format_minutes(secs * 1000)),
    };
    let uses = match info.max_uses {
        None => "不限次数".to_string(),
        Some(n) => format!("限用 {n} 次"),
    };
    format!("{role} · {expires} · {uses}")
}

/// 邀请码红字撤销入口 / 二段确认行。
fn invite_revoke_row(code: String, confirmed: bool) -> Element<'static, Message> {
    if confirmed {
        return container(
            row![
                text("撤销后该邀请码立即失效").size(pal::fs_micro()).color(pal::TEXT_SECONDARY),
                space().width(Length::Fill),
                controls::btn(
                    "确认撤销",
                    &controls::btn_sm(),
                    Variant::Danger,
                    Some(Message::Space(crate::SpaceMsg::RevokeInvite(code))),
                ),
                controls::btn(
                    "取消",
                    &controls::btn_sm(),
                    Variant::Ghost,
                    Some(Message::Space(crate::SpaceMsg::InviteRevokeCancel)),
                ),
            ]
            .spacing(6),
        )
        .padding(Padding::ZERO.horizontal(12.0).vertical(2.0))
        .width(Length::Fill)
        .into();
    }
    container(
        button(
            text("撤销该邀请码").size(pal::fs_micro()).color(pal::DANGER),
        )
        .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
        .style(|_t, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                .then_some(pal::DANGER.scale_alpha(0.12).into()),
            border: border::rounded(pal::RADIUS_XS),
            ..Default::default()
        })
        .on_press(Message::Space(crate::SpaceMsg::InviteRevokeAsk(code))),
    )
    .padding(Padding::ZERO.horizontal(12.0).vertical(2.0))
    .into()
}

/// 邀请码弹层：表单（角色 / 过期小时 / 次数）+ 最近生成结果块 + 已有邀请码列表 + 错误条。
fn space_invite_entries(
    app: &App,
) -> Vec<(usize, MenuEntry<'static, Message>)> {
    use atelyx_core::space_client::SpaceRole;
    let mut entries: Vec<(usize, MenuEntry<Message>)> = vec![
        (
            0,
            MenuEntry::Submenu {
                label: format!("角色：{}", match app.space_invite_form.role {
                    SpaceRole::Owner | SpaceRole::Editor => "编辑者",
                    SpaceRole::Viewer => "只读",
                }),
                icon: None,
                disabled: false,
                pane: 1,
            },
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input(
                    "过期小时（空 = 永久有效）",
                    &app.space_invite_form.hours.clone(),
                    |v| Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::InviteHours, v)),
                    false,
                ),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_form_input(
                    "次数上限（空 = 不限）",
                    &app.space_invite_form.uses.clone(),
                    |v| Message::Space(crate::SpaceMsg::LoginField(crate::SpaceField::InviteUses, v)),
                    false,
                ),
                30.0,
            ),
        ),
        (
            0,
            MenuEntry::Custom(
                space_action_row(
                    if app.space_invite_result.is_some() { "重新生成" } else { "生成邀请码" },
                    app.space_busy,
                    Message::Space(crate::SpaceMsg::CreateInvite),
                ),
                30.0,
            ),
        ),
    ];

    // 最近生成结果块：等宽 code + 复制（点击转「已复制」）+ 元信息 + 红字撤销
    if let Some(info) = &app.space_invite_result {
        let copy_label = if app.space_invite_copied { "已复制" } else { "复制" };
        entries.push((
            0,
            MenuEntry::Custom(
                column![
                    container(
                        row![
                            text(info.code.clone()).size(pal::fs_body()).color(pal::ACCENT).font(mono()),
                            space().width(Length::Fill),
                            controls::btn(
                                copy_label,
                                &controls::btn_sm(),
                                Variant::Secondary,
                                Some(Message::Space(crate::SpaceMsg::InviteCopy(info.code.clone()))),
                            ),
                        ]
                        .spacing(8),
                    )
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill),
                    container(
                        text(invite_meta_line(info)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                    )
                    .padding(Padding::ZERO.horizontal(12.0))
                    .width(Length::Fill),
                    invite_revoke_row(info.code.clone(), app.space_invite_confirm.as_deref() == Some(info.code.as_str())),
                ]
                .spacing(4)
                .padding(Padding::ZERO.vertical(4.0))
                .width(Length::Fill)
                .into(),
                92.0,
            ),
        ));
    }

    // 已有邀请码（owner 端点列出；结果块中的那一条不重复展示）
    if let Some(list) = &app.space_invites {
        let rows: Vec<_> = list
            .iter()
            .filter(|i| app.space_invite_result.as_ref().is_none_or(|r| r.code != i.code))
            .collect();
        if !rows.is_empty() {
            entries.push((0, MenuEntry::Group("已有邀请码".into())));
            for info in rows {
                let confirmed = app.space_invite_confirm.as_deref() == Some(info.code.as_str());
                entries.push((
                    0,
                    MenuEntry::Custom(
                        if confirmed {
                            container(
                                row![
                                    text("撤销该邀请码？").size(pal::fs_caption()).color(pal::TEXT_SECONDARY),
                                    space().width(Length::Fill),
                                    controls::btn(
                                        "确认撤销",
                                        &controls::btn_sm(),
                                        Variant::Danger,
                                        Some(Message::Space(crate::SpaceMsg::RevokeInvite(info.code.clone()))),
                                    ),
                                    controls::btn(
                                        "取消",
                                        &controls::btn_sm(),
                                        Variant::Ghost,
                                        Some(Message::Space(crate::SpaceMsg::InviteRevokeCancel)),
                                    ),
                                ]
                                .spacing(6),
                            )
                            .padding(Padding::ZERO.horizontal(12.0))
                            .width(Length::Fill)
                            .height(Length::Fixed(pal::MENU_ITEM_H))
                            .align_y(Vertical::Center)
                            .into()
                        } else {
                            container(
                                row![
                                    text(info.code.clone()).size(pal::fs_micro()).color(pal::TEXT_PRIMARY).font(mono()),
                                    text(invite_meta_line(info)).size(pal::fs_micro()).color(pal::TEXT_MUTED),
                                    space().width(Length::Fill),
                                    button(
                                        text("撤销").size(pal::fs_micro()).color(pal::DANGER),
                                    )
                                    .padding(Padding::ZERO.horizontal(6.0).vertical(2.0))
                                    .style(|_t, status| button::Style {
                                        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                                            .then_some(pal::DANGER.scale_alpha(0.12).into()),
                                        border: border::rounded(pal::RADIUS_XS),
                                        ..Default::default()
                                    })
                                    .on_press(Message::Space(crate::SpaceMsg::InviteRevokeAsk(info.code.clone()))),
                                ]
                                .spacing(8),
                            )
                            .padding(Padding::ZERO.horizontal(12.0))
                            .width(Length::Fill)
                            .height(Length::Fixed(pal::MENU_ITEM_H))
                            .align_y(Vertical::Center)
                            .into()
                        },
                        pal::MENU_ITEM_H,
                    ),
                ));
            }
        }
    }

    if let Some(err) = &app.space_invites_error {
        entries.push((0, MenuEntry::Custom(popup_error_bar(err), 22.0)));
    } else if app.space_invites_loading && app.space_invites.is_none() {
        entries.push((0, MenuEntry::Custom(space_hint("加载中…"), 18.0)));
    }

    // pane 1 = 角色下拉（服务端仅接受 editor / viewer 两档）
    for (role, label) in [(SpaceRole::Editor, "编辑者"), (SpaceRole::Viewer, "只读")] {
        let pick = Message::Space(crate::SpaceMsg::InviteRolePick(role));
        entries.push((
            1,
            MenuEntry::check(label, app.space_invite_form.role == role, pick),
        ));
    }
    entries
}
