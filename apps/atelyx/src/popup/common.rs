//! 弹层共用小部件：说明行与单行输入。

use iced::widget::container;
use iced::{Element, Length, Padding};

use crate::ui::controls;
use crate::ui::palette as pal;
use crate::ui::text;
use crate::Message;

/// 弹层内联输入（单行；提交由菜单项动作携带草稿）。
pub(super) fn popup_text_input(
    placeholder: &str,
    draft: &str,
    on_input: impl Fn(String) -> Message + 'static,
) -> Element<'static, Message> {
    iced::widget::text_input(placeholder, draft)
        .size(pal::fs_caption())
        .font(crate::ui::regular())
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .on_input(on_input)
        .style(|_theme, status| controls::form_input_style(status))
        .into()
}

pub(super) fn popup_hint(text_content: &str) -> Element<'static, Message> {
    container(text(text_content.to_string()).size(pal::fs_micro()).color(pal::TEXT_MUTED))
        .padding(Padding::ZERO.horizontal(12.0))
        .width(Length::Fill)
        .into()
}
