//! 悬停全文提示的接线：各视图只给悬停区与标签，进出消息由这一处产生。

use iced::widget::MouseArea;
use iced::window;

use crate::ui::UiMsg;
use crate::Message;

/// 给已挂好按下 / 双击 / 右键处理的悬停区补全文提示；`box_px` 为触发区边长
/// （提示锚在光标下方半边长 + 4）。
pub(crate) fn with_tip<'a>(
    area: MouseArea<'a, Message>,
    window: window::Id,
    label: impl Into<String>,
    box_px: f32,
) -> MouseArea<'a, Message> {
    let label = label.into();
    area.on_enter(Message::Ui(UiMsg::HoverTip { window, label: label.clone(), box_px }))
        .on_exit(Message::Ui(UiMsg::HoverTipEnd { window, label }))
}
