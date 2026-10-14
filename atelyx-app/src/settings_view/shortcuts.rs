//! 设置-快捷键：可重映射命令的键位列表（按作用域分组；录制改键 / 恢复默认 / 同作用域冲突警示）。

use iced::alignment::Vertical;
use iced::widget::{column, container, mouse_area, row, scrollable, space, text};
use iced::{border, Element, Length, Padding};

use crate::commands::{self, Scope};
use crate::settings_view::group_head;
use crate::ui::controls;
use crate::ui::palette as pal;
use crate::ui::UiMsg;
use crate::{App, Message};

pub fn view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let resolved = commands::resolve(&app.global.command_shortcuts);
    let mut col = column![].spacing(8).padding(Padding::ZERO.horizontal(20.0));
    col = col.push(group_head("命令键位", "", false));
    col = col.push(
        text("点击键串框录制新键位；冲突时先登记者生效")
            .size(pal::fs_micro())
            .color(pal::TEXT_MUTED),
    );
    // 领域分组卡片（组序随命令注册表：笔记在前；空领域不出组）
    for scope in [Scope::Note, Scope::Table] {
        if resolved.iter().any(|(id, _)| id.scope() == scope) {
            col = col.push(scope_group(app, window, scope, &resolved));
        }
    }
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 领域分组卡片：组标题行 + 组内命令行（归属由分组标题承载，行内不再标领域）。
fn scope_group(
    app: &App,
    window: iced::window::Id,
    scope: Scope,
    resolved: &[(commands::CommandId, String)],
) -> Element<'static, Message> {
    let mut inner = column![
        text(scope.label())
            .size(pal::fs_caption())
            .color(pal::TEXT_MUTED),
    ]
    .spacing(2)
    .padding(Padding::ZERO.horizontal(12.0).top(8.0).bottom(4.0));
    for (id, binding) in resolved.iter().filter(|(id, _)| id.scope() == scope) {
        inner = inner.push(command_row(app, window, *id, binding, resolved));
    }
    container(inner)
        .width(Length::Fill)
        .style(|_t| iced::widget::container::Style {
            background: Some(pal::BG_TERTIARY.into()),
            border: border::rounded(pal::RADIUS_MD),
            ..iced::widget::container::Style::default()
        })
        .into()
}

/// 单命令行：命令名，右侧键串输入框（点击进入录制）+ 冲突警示 + 恢复默认。
fn command_row(
    app: &App,
    window: iced::window::Id,
    id: commands::CommandId,
    binding: &str,
    resolved: &[(commands::CommandId, String)],
) -> Element<'static, Message> {
    let recording = app.shortcut_recording == Some(id);
    // 键串框呈表单输入形态：默认态描边 BORDER_STRONG，录制（= 聚焦）态金描边
    let (label, label_color) = if recording {
        ("按下组合键，Esc 取消".to_string(), pal::ACCENT)
    } else {
        (display_keys(binding), pal::TEXT_PRIMARY)
    };
    let key_box = container(
        text(label)
            .size(pal::fs_micro())
            .color(label_color)
            .font(if recording { iced::Font::default() } else { crate::layout_view::mono() }),
    )
    .width(Length::Fixed(150.0))
    .padding(Padding::ZERO.horizontal(10.0).vertical(5.0))
    .style(move |_t| iced::widget::container::Style {
        background: Some(pal::BG_TERTIARY.into()),
        border: border::rounded(pal::RADIUS_SM)
            .color(if recording { pal::ACCENT } else { pal::BORDER_STRONG })
            .width(1.0),
        ..iced::widget::container::Style::default()
    });
    // 录制态不响应点击（等待组合键），非录制态点击 = 进入录制
    let key_input: Element<'static, Message> = if recording {
        key_box.into()
    } else {
        mouse_area(key_box).on_press(Message::ShortcutRecord(id)).into()
    };
    let mut r = row![].spacing(8).align_y(Vertical::Center);
    r = r.push(key_input);
    if let Some(mark) = conflict_mark(window, resolved, id) {
        r = r.push(mark);
    }
    // 有覆盖才可恢复默认
    if app.global.command_shortcuts.contains_key(id.key()) {
        r = r.push(controls::btn(
            "恢复默认",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::ShortcutReset(id)),
        ));
    }
    container(
        row![
            text(id.label())
                .size(pal::fs_body())
                .color(pal::TEXT_PRIMARY)
                .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
            space().width(Length::Fill),
            r,
        ]
        .spacing(10)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::ZERO.vertical(8.0))
    .into()
}

/// 同领域键串冲突警示：底色随 UiTick 心跳两态交替（呼吸感），悬停列出相撞命令。
fn conflict_mark(
    window: iced::window::Id,
    resolved: &[(commands::CommandId, String)],
    id: commands::CommandId,
) -> Option<Element<'static, Message>> {
    let others = commands::conflicts(resolved, id);
    if others.is_empty() {
        return None;
    }
    let names: Vec<String> = others.iter().map(|c| c.label().to_string()).collect();
    let tip = format!("与 {} 冲突，按键时先登记的命令生效", names.join("、"));
    let bg = if crate::now_secs().is_multiple_of(2) { pal::ACCENT_SOFT } else { pal::BG_TERTIARY };
    Some(
        mouse_area(controls::badge("冲突", bg, pal::WARNING))
            .on_enter(Message::Ui(UiMsg::HoverTip { window, label: tip.clone(), box_px: 24.0 }))
            .on_exit(Message::Ui(UiMsg::HoverTipEnd { window, label: tip }))
            .into(),
    )
}

/// 键串友好展示：mod → Ctrl，片段首字母大写（Ctrl+Z / Shift+Delete）。
fn display_keys(binding: &str) -> String {
    let Some(b) = commands::parse(binding) else {
        return binding.to_string();
    };
    let mut parts: Vec<String> = Vec::with_capacity(4);
    if b.mod_key {
        parts.push("Ctrl".into());
    }
    if b.alt {
        parts.push("Alt".into());
    }
    if b.shift {
        parts.push("Shift".into());
    }
    let mut chars = b.key.chars();
    parts.push(match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => b.key,
    });
    parts.join("+")
}
