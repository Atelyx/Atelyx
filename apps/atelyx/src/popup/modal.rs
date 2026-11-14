//! 居中模态：内容由各自 overlay 渲染，这里只给宽度。

use crate::layout_view::PopupState;

pub(super) fn width(state: &PopupState) -> f32 {
    match state {
        // 居中模态不走锚点几何，估高由各自 overlay 钳制
        PopupState::HistoryBrowser { .. } => crate::history_browser::CARD_WIDTH,
        PopupState::TableAiField { .. } => crate::table_ai_view::MODAL_WIDTH,
        _ => unreachable!("宽度按域分派，本域以外的变体不会到这里"),
    }
}
