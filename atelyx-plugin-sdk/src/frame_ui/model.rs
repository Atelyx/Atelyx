//! 控件层跨帧状态：面尺寸、上次布局、悬停 / 按下 / 焦点、各输入的编辑态与滚动偏移。
//! 布局记录是输入路由的唯一依据——宿主只回发命中 id 与坐标，语义由这里解释。

use std::collections::HashMap;
use std::sync::Arc;

use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::MarkdownBlock;
use atelyx_plugin_protocol::paint::{TextFamily, TextWeight};

use super::geom::Rect;
use super::markdown;
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
    /// 多行输入框：`inner` = 文字区（已扣除内边距），软换行与显式换行都按它排布。
    /// `enter_submits` 为真时回车提交（Shift+回车换行），否则回车插入换行。
    TextArea { inner: Rect, font: FontSpec, value: String, enter_submits: bool },
    Select { options: usize },
    Checkbox,
    /// `content` = 内容总高（可滚动余量 = 内容高 - 视口高）。
    Scroll { viewport: Rect, content: f32 },
    /// 展开的下拉选项行（内部命中 id，插件看不到）。
    Option { select: String, index: usize },
    /// 滚动条：按下 / 拖动按纵向落点换算偏移（内部命中 id，插件看不到）。
    ScrollBar { scroll: String, track: Rect, thumb: f32, max_offset: f32 },
    /// 自绘子区：语义与按钮一致（按下再抬起算一次点击）。
    Custom,
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
    /// 正在拖动的滚动条（滚动容器 id）：移动时按纵向落点改偏移，抬起即解除。
    pub drag_bar: Option<String>,
    pub focus: Option<String>,
    pub inputs: HashMap<String, InputState>,
    pub selects: HashMap<String, SelectState>,
    pub scroll: HashMap<String, f32>,
    /// 各滚动容器上一帧的可滚动余量（贴底跟随据此判断「上一帧是否停在终点」）。
    pub scroll_max: HashMap<String, f32>,
    /// 各滚动容器上一帧是否声明贴底跟随（由关转开的那一帧跳到终点）。
    pub follow_prev: HashMap<String, bool>,
    /// `markdown.parse` 的结果缓存（按原文取，内容不变即不重取）。
    pub markdown: HashMap<String, Vec<MarkdownBlock>>,
    /// Markdown 版面缓存（键含内容、宽度与量算代次）：滚动时长内容不必每帧重排。
    pub markdown_layouts: Arc<markdown::LayoutCache>,
    /// 上次已提交的帧与所在面：同内容帧不再提交（宿主每收一帧都要重投影并整窗重绘）。
    /// 面重新挂载时宿主会再发一次 `Resize`，据此清空——否则重挂后同内容帧被吞，面会停在空态。
    pub submitted: Option<(String, SurfaceFrame)>,
    /// 尺寸刚变、下一帧还没交：让 `render` 先按估算字宽交一帧，不再等量算往返。
    pub resize_pending: bool,
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
            drag_bar: None,
            focus: None,
            inputs: HashMap::new(),
            selects: HashMap::new(),
            scroll: HashMap::new(),
            scroll_max: HashMap::new(),
            follow_prev: HashMap::new(),
            markdown: HashMap::new(),
            markdown_layouts: Arc::default(),
            submitted: None,
            resize_pending: false,
        }
    }
}

impl UiState {
    /// 删掉本帧已不存在控件的状态：长期运行的插件不该按历史控件累积。
    pub(crate) fn prune(&mut self) {
        self.inputs.retain(|id, _| self.layouts.contains_key(id));
        self.selects.retain(|id, _| self.layouts.contains_key(id));
        self.scroll.retain(|id, _| self.layouts.contains_key(id));
        self.scroll_max.retain(|id, _| self.layouts.contains_key(id));
        self.follow_prev.retain(|id, _| self.layouts.contains_key(id));
        if self.drag_bar.as_ref().is_some_and(|id| !self.layouts.contains_key(id)) {
            self.drag_bar = None;
        }
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
