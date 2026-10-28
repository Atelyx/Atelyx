//! 控件层跨帧状态：面尺寸、上次布局、悬停 / 按下 / 焦点、各输入的编辑态与滚动偏移。
//! 布局记录是输入路由的唯一依据——宿主只回发命中 id 与坐标，语义由这里解释。

use std::collections::HashMap;

use atelyx_plugin_protocol::view::{TextFamily, TextWeight};

use super::geom::Rect;
use super::metrics::Metrics;
use super::theme::Theme;

/// 输入框架内文字的火型：字号 + 字重 + 字体族。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FontSpec {
    pub size: f32,
    pub weight: Option<TextWeight>,
    pub family: Option<TextFamily>,
}

/// 一个控件在上一帧的落位与解释所需的信息。
#[derive(Clone, Debug)]
pub(crate) enum Layout {
    Button,
    /// `inner` = 文字可写区域（已扣除水平内边距），值取绘制时的快照。
    Input { inner: Rect, font: FontSpec, value: String },
    Select { options: usize },
    Checkbox,
    /// `stops` = 内容块边界（滚动位置只落这些位置，避免半块露出）。
    Scroll { viewport: Rect, content: f32, stops: Vec<f32> },
    /// 展开的下拉选项行（内部命中 id，插件看不到）。
    Option { select: String, index: usize },
}

/// 单个输入框的编辑态（文本本身由插件持有，这里只记光标与选区）。
#[derive(Clone, Debug, Default)]
pub(crate) struct InputState {
    /// 光标字节位（总在字符边界上）。
    pub caret: usize,
    /// 选区锚点字节位（与光标相同 = 无选区）。
    pub anchor: usize,
    /// 输入法组合串（未提交，不进入文本值）。
    pub preedit: String,
}

/// 下拉的展开态与键盘巡航项。
#[derive(Clone, Debug, Default)]
pub(crate) struct SelectState {
    pub open: bool,
    pub hover: Option<usize>,
}

/// 按下中的目标：抬起时只有同一控件才算一次点击。
#[derive(Clone, Debug)]
pub(crate) struct Press {
    pub id: String,
    /// 按下落在输入框：移动时按坐标延伸选区。
    pub from_input: bool,
}

pub(crate) struct UiState {
    pub size: (f32, f32),
    pub theme: Option<Theme>,
    pub metrics: Metrics,
    pub layouts: HashMap<String, Layout>,
    pub focusables: Vec<String>,
    pub hovered: Option<String>,
    pub press: Option<Press>,
    pub focus: Option<String>,
    pub inputs: HashMap<String, InputState>,
    pub selects: HashMap<String, SelectState>,
    pub scroll: HashMap<String, f32>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            // 宿主在首次绘制时才回发真实尺寸，这里先给一个可用占位
            size: (320.0, 400.0),
            theme: None,
            metrics: Metrics::default(),
            layouts: HashMap::new(),
            focusables: Vec::new(),
            hovered: None,
            press: None,
            focus: None,
            inputs: HashMap::new(),
            selects: HashMap::new(),
            scroll: HashMap::new(),
        }
    }
}

impl UiState {
    /// 删掉本帧已不存在控件的状态：长期运行的插件不该按历史控件累积。
    pub(crate) fn prune(&mut self) {
        self.inputs.retain(|id, _| self.layouts.contains_key(id));
        self.selects.retain(|id, _| self.layouts.contains_key(id));
        self.scroll.retain(|id, _| self.layouts.contains_key(id));
        if self.focus.as_ref().is_some_and(|id| !self.layouts.contains_key(id)) {
            self.focus = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruning_drops_state_of_widgets_gone_from_the_last_frame() {
        let mut state = UiState { focus: Some("gone".into()), ..UiState::default() };
        state.inputs.insert("gone".into(), InputState::default());
        state.selects.insert("gone".into(), SelectState { open: true, hover: None });
        state.scroll.insert("gone".into(), 12.0);
        state.layouts.insert("kept".into(), Layout::Button);
        state.prune();
        assert!(state.inputs.is_empty() && state.selects.is_empty() && state.scroll.is_empty());
        assert!(state.focus.is_none(), "焦点不能留在已消失的控件上");
    }
}
