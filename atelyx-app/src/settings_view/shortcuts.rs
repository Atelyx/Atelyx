//! 设置-快捷键：可重映射命令的键位列表（录制改键 / 恢复默认 / 同作用域冲突警示）。

use iced::alignment::Vertical;
use iced::widget::{column, container, mouse_area, row, scrollable, space, text};
use iced::{Element, Length, Padding};

use crate::commands::{self, CommandId, Scope};
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
        text("点「修改」录制新键位；冲突时先登记者生效")
            .size(pal::fs_micro())
            .color(pal::TEXT_MUTED),
    );
    for (id, binding) in &resolved {
        col = col.push(command_row(app, window, *id, binding, &resolved));
    }
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 单命令行：作用域徽标 + 命令名，右侧当前键串 + 冲突警示 + 修改 / 恢复默认。
fn command_row(
    app: &App,
    window: iced::window::Id,
    id: CommandId,
    binding: &str,
    resolved: &[(CommandId, String)],
) -> Element<'static, Message> {
    let control: Element<'static, Message> = if app.shortcut_recording == Some(id) {
        text("按下组合键，Esc 取消").size(pal::fs_caption()).color(pal::ACCENT).into()
    } else {
        let mut r = row![].spacing(8).align_y(Vertical::Center);
        r = r.push(controls::kbd(display_keys(binding)));
        if let Some(mark) = conflict_mark(window, resolved, id) {
            r = r.push(mark);
        }
        r = r.push(controls::btn(
            "修改",
            &controls::btn_sm(),
            controls::Variant::Ghost,
            Some(Message::ShortcutRecord(id)),
        ));
        // 有覆盖才可恢复默认
        if app.global.command_shortcuts.contains_key(id.key()) {
            r = r.push(controls::btn(
                "恢复默认",
                &controls::btn_sm(),
                controls::Variant::Ghost,
                Some(Message::ShortcutReset(id)),
            ));
        }
        r.into()
    };
    crate::settings_view::card(
        row![
            scope_badge(id.scope()),
            text(id.label())
                .size(pal::fs_body())
                .color(pal::TEXT_PRIMARY)
                .font(iced::Font { weight: iced::font::Weight::Medium, ..iced::Font::default() }),
            space().width(Length::Fill),
            control,
        ]
        .spacing(10)
        .align_y(Vertical::Center),
    )
}

/// 作用域徽标（笔记 / 表格）。
fn scope_badge(scope: Scope) -> Element<'static, Message> {
    let (label, color) = match scope {
        Scope::Note => ("笔记", pal::ACCENT),
        Scope::Table => ("表格", pal::TEXT_SECONDARY),
    };
    controls::badge(label, pal::BG_SUNKEN, color)
}

/// 同作用域键串冲突警示：底色随 UiTick 心跳两态交替（呼吸感），悬停列出相撞命令。
fn conflict_mark(
    window: iced::window::Id,
    resolved: &[(CommandId, String)],
    id: CommandId,
) -> Option<Element<'static, Message>> {
    let others = commands::conflicts(resolved, id);
    if others.is_empty() {
        return None;
    }
    let names: Vec<String> = others.iter().map(|c| c.label().to_string()).collect();
    let tip = format!("与 {} 冲突，按键时先登记的命令生效", names.join("、"));
    let bg = if crate::now_secs() % 2 == 0 { pal::ACCENT_SOFT } else { pal::BG_TERTIARY };
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
