//! 设置-编辑器：宽松换行 / 页面内标题 / 正文行宽（应用级阅读偏好）。

use iced::widget::{column, container, row, scrollable, text};
use iced::{Element, Length, Padding};

use crate::settings_view::{group_head, setting_row};
use crate::ui::controls;
use crate::ui::palette as pal;
use crate::{App, Message};

pub fn view(app: &App) -> Element<'static, Message> {
    let width_draft = app
        .content_width_draft
        .clone()
        .unwrap_or_else(|| app.global.content_width.to_string());
    let width_input = row![
        controls::number_input(
            &width_draft,
            72.0,
            Message::ContentWidthInput,
            Message::ContentWidthCommit,
        ),
        text("0 = 不限").size(pal::fs_micro()).color(pal::TEXT_MUTED),
    ]
    .spacing(6)
    .align_y(iced::alignment::Vertical::Center);
    let mut col = column![].spacing(16).padding(Padding::ZERO.horizontal(20.0));
    col = col.push(group_head("阅读", "global.json", false));
    col = col.push(setting_row(
        "宽松换行",
        "单个换行渲染为换行；关闭按 Markdown 标准需空行换行",
        controls::toggle(app.global.soft_wrap, Some(Message::SoftWrapToggle)),
    ));
    col = col.push(setting_row(
        "页面内标题",
        "正文顶部显示文件名，点击可重命名",
        controls::toggle(app.global.in_page_title, Some(Message::InPageTitleToggle)),
    ));
    col = col.push(group_head("正文", "global.json", false));
    col = col.push(setting_row("正文行宽", "正文区最大宽度（px）", width_input.into()));
    scrollable(container(col).padding(Padding::ZERO.top(4.0).bottom(24.0)))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
