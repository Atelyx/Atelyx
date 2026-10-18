//! 设置-通用：界面字体 / 界面字号 / 自动恢复 / 自动更新 / 开机自启（应用级）。

use iced::widget::{column, container, scrollable};
use iced::{Element, Length, Padding};

use crate::layout_view::PopupState;
use crate::settings_view::{group_head, setting_row};
use crate::ui::controls;
use crate::{App, Message};

/// 字体族下拉触发钮宽（族名长短不一，给足一屏可读宽度）。
const TRIGGER_W: f32 = 200.0;

pub fn view(app: &App, window: iced::window::Id) -> Element<'static, Message> {
    let font_draft = app
        .font_size_draft
        .clone()
        .unwrap_or_else(|| app.global.font_size.to_string());
    let autostart_on = app.autostart_enabled.unwrap_or(false);
    let font_label = interface_font_label(app.global.font_family.as_deref());
    let mut col = column![].spacing(8).padding(Padding::ZERO.horizontal(20.0));
    col = col.push(group_head("界面", "", false));
    col = col.push(setting_row(
        "界面字体",
        "界面文字的字体族，默认跟随系统",
        controls::dropdown_trigger(
            &font_label,
            TRIGGER_W,
            Some(super::open_popup(app, window, PopupState::FontPicker { for_table: false })),
        ),
    ));
    col = col.push(setting_row(
        "界面字号",
        "界面文字的基准字号（12–20）",
        controls::number_input(&font_draft, 64.0, Message::FontSizeInput, Message::FontSizeCommit),
    ));
    col = col.push(group_head("启动", "", false));
    col = col.push(setting_row(
        "自动恢复",
        "启动时恢复上次打开的文件",
        controls::toggle(app.global.auto_restore, Some(Message::AutoRestoreToggle)),
    ));
    col = col.push(setting_row(
        "自动检查更新",
        "启动时静默检查新版本",
        controls::toggle(app.global.auto_update, Some(Message::AutoUpdateToggle)),
    ));
    col = col.push(group_head("系统", "", false));
    col = col.push(setting_row(
        "开机自启",
        "登录系统时自动启动",
        controls::toggle(autostart_on, Some(Message::AutostartToggle)),
    ));
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 界面字体行标签：跨机搬来的旧值在本机没装时如实标注。
fn interface_font_label(name: Option<&str>) -> String {
    match name {
        None => "跟随系统".to_string(),
        Some(n) if crate::ui::font::is_installed(n) => n.to_string(),
        Some(n) => format!("{n}（未安装）"),
    }
}
