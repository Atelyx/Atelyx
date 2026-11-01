//! 控件层跨帧状态：面尺寸、上次布局、悬停 / 按下 / 焦点、各输入的编辑态与滚动偏移。
//! 布局记录是输入路由的唯一依据——宿主只回发命中 id 与坐标，语义由这里解释。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::paint::{TextFamily, TextWeight};

use super::geom::Rect;
use super::markdown;
use super::metrics::Metrics;
use super::theme::Theme;

/// 光标闪烁半周期（毫秒）：静默时先显 500ms、再隐 500ms。
pub(crate) const BLINK_MS: u128 = 500;

/// 悬停提示的停留时间：指针在同一控件上停够这么久才出提示（与宿主同一节拍）。
pub(crate) const HINT_DELAY: Duration = Duration::from_millis(400);

/// 静默 `elapsed` 之后光标是否显形。任何编辑 / 光标移动 / 重新聚焦都会把计时清零，
/// 所以按键的瞬间光标一定是亮的。
pub(crate) fn caret_lit(elapsed: Duration) -> bool {
    elapsed.as_millis() % (BLINK_MS * 2) < BLINK_MS
}

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
    /// `marks` = 本帧的标记区间（字节），点进去整段选中、边界退格整段删除。
    TextArea {
        inner: Rect,
        font: FontSpec,
        value: String,
        enter_submits: bool,
        marks: Vec<(usize, usize)>,
    },
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
    /// 控件层自持的复制钮（代码块头）：按下即写剪贴板，插件不必知道这次点击。
    Copy { text: String },
    /// 只读 Markdown 块：区内可拖选文字（`rect` 是块的外框，落点按版面换算到行列）。
    Markdown { rect: Rect },
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
    /// 多行输入的纵向滚动偏移（像素；单行输入恒为 0）。
    pub scroll: f32,
    /// 撤销栈（(文本, 光标) 快照；文本变了才压栈）。
    pub undo: Vec<(String, usize)>,
    /// 重做栈（撤销时压入；新编辑作废）。
    pub redo: Vec<(String, usize)>,
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

/// 自绘子区的拖拽会话：位移越过阈值才算开始，`dx` / `dy` 是从按下点的累计位移。
#[derive(Clone, Debug)]
pub(crate) struct Drag {
    pub id: String,
    pub origin: (f32, f32),
    pub dx: f32,
    pub dy: f32,
    pub started: bool,
}

/// 一段文本选择：块 id + 起点 / 终点（行序，行内字节位）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextSelection {
    pub id: String,
    pub anchor: (usize, usize),
    pub focus: (usize, usize),
}

impl TextSelection {
    /// 选择区间的规范序（起点在前）。
    pub(crate) fn range(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

/// 悬停提示的待发状态：指针停住的那个控件与起始时刻。
#[derive(Clone, Debug)]
pub(crate) struct HintHover {
    pub id: String,
    pub since: Instant,
}

pub(crate) struct UiState {
    pub size: (f32, f32),
    pub theme: Option<Theme>,
    /// 帧拍计数（[`super::FrameUi::tick`] 推进）：自转指示这类动画的相位由它算。
    pub clock: u32,
    /// 光标闪烁的计时起点（最近一次编辑 / 光标移动 / 聚焦）。
    pub blink_at: Instant,
    /// 本帧声明的悬停提示（控件 id → 提示文字），输入路由按上一帧的这份表决定要不要出提示。
    pub hints: HashMap<String, String>,
    /// 指针停住的那个控件与起始时刻（停够 [`HINT_DELAY`] 即出提示）。
    pub hint: Option<HintHover>,
    /// 当前文本选择（只在一个 Markdown 块内）。
    pub selection: Option<TextSelection>,
    /// 各 Markdown 块最近一次绘制的版面：选择命中与复制按它算。
    pub markdown_last: HashMap<String, Arc<markdown::Layout>>,
    /// 最近一次指针位置（面本地）：提示贴指针下沿画。
    pub pointer: Option<(f32, f32)>,
    pub metrics: Metrics,
    pub layouts: HashMap<String, Layout>,
    pub focusables: Vec<String>,
    pub hovered: Option<String>,
    pub press: Option<Press>,
    /// 正在拖动的滚动条（滚动容器 id）：移动时按纵向落点改偏移，抬起即解除。
    pub drag_bar: Option<String>,
    /// 按下在自绘子区上的拖拽候选：移动越过阈值后上报拖拽事件。
    pub drag: Option<Drag>,
    /// 上一帧光标声明为拖动形态的子区：只有按下这些子区才建拖拽候选。
    pub draggables: std::collections::HashSet<String>,
    pub focus: Option<String>,
    pub inputs: HashMap<String, InputState>,
    pub selects: HashMap<String, SelectState>,
    pub scroll: HashMap<String, f32>,
    /// 各滚动容器上一帧的可滚动余量（贴底跟随据此判断「上一帧是否停在终点」）。
    pub scroll_max: HashMap<String, f32>,
    /// 各滚动容器上一帧是否声明贴底跟随（由关转开的那一帧跳到终点）。
    pub follow_prev: HashMap<String, bool>,
    /// Markdown 版面缓存（键含内容、宽度与量算代次）：滚动时长内容不必每帧重排。
    pub markdown_layouts: Arc<markdown::LayoutCache>,
    /// 上次已提交的帧与所在面：同内容帧不再提交（宿主每收一帧都要重投影并整窗重绘）。
    /// 面重新挂载时宿主会再发一次 `Resize`，据此清空——否则重挂后同内容帧被吞，面会停在空态。
    pub submitted: Option<(String, SurfaceFrame)>,
    /// 尺寸刚变、下一帧还没交：让 `render` 先按估算字宽交一帧，不再等量算往返。
    pub resize_pending: bool,
    /// 上一帧声明的浮层区域：有浮层时滚轮只作用于浮层内的滚动区。
    pub overlays: Vec<Rect>,
    /// 浮层内的滚动容器 id：浮层打开时不把滚轮落到下层列表。
    pub overlay_scrolls: std::collections::HashSet<String>,
    /// 虚拟列表各条目的最近一次实测高（列表 id + 条目 key）：只用于估算屏外条目，
    /// 改宽后优先按新宽度重排屏内条目。
    pub virtual_heights: Arc<Mutex<HashMap<(String, u64), f32>>>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            // 宿主在首次绘制时才回发真实尺寸，这里先给一个可用占位
            size: (320.0, 400.0),
            theme: None,
            clock: 0,
            blink_at: Instant::now(),
            hints: HashMap::new(),
            hint: None,
            pointer: None,
            selection: None,
            markdown_last: HashMap::new(),
            metrics: Metrics::default(),
            layouts: HashMap::new(),
            focusables: Vec::new(),
            hovered: None,
            press: None,
            drag_bar: None,
            drag: None,
            draggables: std::collections::HashSet::new(),
            focus: None,
            inputs: HashMap::new(),
            selects: HashMap::new(),
            scroll: HashMap::new(),
            scroll_max: HashMap::new(),
            follow_prev: HashMap::new(),
            markdown_layouts: Arc::default(),
            submitted: None,
            resize_pending: false,
            overlays: Vec::new(),
            overlay_scrolls: std::collections::HashSet::new(),
            virtual_heights: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl UiState {
    /// 光标重新开始闪烁（从显形起算）：编辑、光标移动、重新聚焦都调它。
    pub(crate) fn reset_blink(&mut self) {
        self.blink_at = Instant::now();
    }

    /// 指针换到另一个控件：提示重新计时；离开提示控件即收起。
    pub(crate) fn track_hint(&mut self, id: Option<&str>) {
        let next = id.map(str::to_string);
        if self.hint.as_ref().map(|hint| hint.id.as_str()) == next.as_deref() {
            return;
        }
        self.hint = next.map(|id| HintHover { id, since: Instant::now() });
    }

    /// 收起提示（指针离开面 / 面失焦）。
    pub(crate) fn clear_hint(&mut self) {
        self.hint = None;
        self.pointer = None;
    }

    /// 此刻该画的提示文字：停够 [`HINT_DELAY`] 才算数。
    pub(crate) fn hint_label(&self) -> Option<String> {
        let hint = self.hint.as_ref()?;
        if hint.since.elapsed() < HINT_DELAY {
            return None;
        }
        self.hints.get(&hint.id).cloned().filter(|text| !text.is_empty())
    }

    /// 提示还在等待到点：驱动据此在等待期间要求帧拍。
    pub(crate) fn hint_pending(&self) -> bool {
        self.hint.as_ref().is_some_and(|hint| hint.since.elapsed() < HINT_DELAY)
    }

    /// 复制选择区内的文字（按版面逐行取，不含 Markdown 标记）。
    pub(crate) fn selection_text(&self) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let layout = self.markdown_last.get(&selection.id)?;
        let text = markdown::selected_text(layout, selection.range());
        (!text.trim().is_empty()).then_some(text)
    }

    /// 光标此刻是否显形。
    pub(crate) fn caret_lit(&self) -> bool {
        caret_lit(self.blink_at.elapsed())
    }

    /// 面重挂：主题与依赖主题的版面缓存作废，尺寸与交互态保留（首帧不按占位尺寸排）。
    pub(crate) fn refresh_theme(&mut self) {
        self.theme = None;
        self.submitted = None;
        self.markdown_layouts = Arc::default();
        self.virtual_heights = Arc::new(Mutex::new(HashMap::new()));
    }

    /// 删掉本帧已不存在控件的状态：长期运行的插件不该按历史控件累积。
    pub(crate) fn prune(&mut self) {
        self.inputs.retain(|id, _| self.layouts.contains_key(id));
        self.selects.retain(|id, _| self.layouts.contains_key(id));
        self.scroll.retain(|id, _| self.layouts.contains_key(id));
        self.scroll_max.retain(|id, _| self.layouts.contains_key(id));
        self.follow_prev.retain(|id, _| self.layouts.contains_key(id));
        self.draggables.retain(|id| self.layouts.contains_key(id));
        self.overlay_scrolls.retain(|id| self.layouts.contains_key(id));
        if self.drag_bar.as_ref().is_some_and(|id| !self.layouts.contains_key(id)) {
            self.drag_bar = None;
        }
        if self.drag.as_ref().is_some_and(|drag| !self.layouts.contains_key(&drag.id)) {
            self.drag = None;
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

    /// 静默半秒显、再半秒隐，循环往复；起点（刚编辑完）必然是亮的。
    #[test]
    fn caret_blinks_on_a_one_second_cycle() {
        assert!(caret_lit(Duration::from_millis(0)));
        assert!(caret_lit(Duration::from_millis(499)));
        assert!(!caret_lit(Duration::from_millis(500)));
        assert!(!caret_lit(Duration::from_millis(999)));
        assert!(caret_lit(Duration::from_millis(1000)));
        assert!(!caret_lit(Duration::from_millis(1500)), "第二秒的后半拍是暗相");
        assert!(caret_lit(Duration::from_millis(2001)));
    }

    /// 编辑与光标移动把计时清零：光标立刻回到显形相位。
    #[test]
    fn editing_restarts_the_blink_cycle() {
        let mut state = UiState { blink_at: Instant::now() - Duration::from_millis(700), ..UiState::default() };
        assert!(!state.caret_lit());
        state.reset_blink();
        assert!(state.caret_lit());
    }
}
