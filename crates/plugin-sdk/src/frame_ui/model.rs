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

/// 只读选区右键菜单里「复制」这一项的命中 id：控件层自持，插件看不到。
pub(crate) const COPY_MENU_ID: &str = "@sdk:copy-menu";

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

/// 缩放浮层：内容按基础坐标排版与记录布局，绘制 / 命中按 `scale` 倍乘，输入按倍率反算。
/// 屏幕坐标 = 基础坐标 × 倍率（原点为面板左上角），所以多个浮层的基础矩形仍在同一坐标系，
/// 命中不会因为浮层的基础矩形重叠而落到别的浮层。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScaledRegion {
    /// 屏幕外框（浮层绘制区域）。
    pub screen: Rect,
    pub scale: f32,
}

impl ScaledRegion {
    /// 屏幕坐标 → 基础坐标。
    pub(crate) fn to_base(&self, x: f32, y: f32) -> (f32, f32) {
        (x / self.scale, y / self.scale)
    }
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
        /// 内部排版行高（光标落点与绘制同一口径）。
        line: f32,
    },
    Select { options: usize },
    Checkbox,
    /// `content` = 内容总高（可滚动余量 = 内容高 - 视口高）。
    Scroll { viewport: Rect, content: f32 },
    /// 展开的下拉选项行（内部命中 id，插件看不到）。
    Option { select: String, index: usize },
    /// 滚动条：按下 / 拖动按纵向落点换算偏移（内部命中 id，插件看不到）。
    ScrollBar { scroll: String, track: Rect, thumb: f32, max_offset: f32 },
    /// 横向滚动条：`owner` = 横向滚动的 Markdown 块 id（偏移记在 `scroll_x`）。
    ScrollBarX { owner: String, track: Rect, thumb: f32, max_offset: f32 },
    /// 自绘子区：语义与按钮一致（按下再抬起算一次点击）。
    Custom,
    /// 控件层自持的复制钮（代码块头）：按下即写剪贴板，插件不必知道这次点击。
    Copy { text: String },
    /// 控件层自持的「展开 / 收起」行：点击就地翻转折叠态，不交给插件。
    Expand { key: String },
    /// Markdown 内的链接片段：`target` 是内链目标或外链 URL，点击交给插件处理。
    Link { target: String },
    /// 只读 Markdown 块：区内可拖选文字（`rect` 是块的外框，落点按版面换算到行列）。
    /// `content_w` = 内容的自然宽：大于外框宽时块内可横向滚动。
    Markdown { rect: Rect, content_w: f32 },
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
    /// 按下点（面本地坐标）：拖出输入面转跨块选区后，锚点按它重建。
    pub origin: (f32, f32),
    /// 跨块选区由「拖出输入面」转换而来：锚点是邻块边界的近似值，待按压面
    /// 重现为 Markdown 块（同 id）后按按下点重建精确锚点。
    pub converted: bool,
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

/// 只读正文里的一个落点：块 id + 行序 + 行内字节位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockPos {
    pub id: String,
    pub line: usize,
    pub byte: usize,
}

/// 一段文本选择：两个落点可跨块。块的先后由本帧绘制序（`markdown_order`）决定。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextSelection {
    pub anchor: BlockPos,
    pub focus: BlockPos,
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
    /// 当前文本选择（两个落点可跨块）。
    pub selection: Option<TextSelection>,
    /// 只读选区右键菜单的落点（面本地坐标）：`Some` 时下一帧画出「复制」项。
    pub copy_menu: Option<(f32, f32)>,
    /// 各 Markdown 块最近一次绘制的版面：选择命中与复制按它算。
    pub markdown_last: HashMap<String, Arc<markdown::Layout>>,
    /// 最近一帧 Markdown 块的绘制序：跨块选择的先后按它比较。
    pub markdown_order: Vec<String>,
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
    /// 各 Markdown 块的横向滚动偏移（自然宽超出外框宽时才有意义）。
    pub scroll_x: HashMap<String, f32>,
    /// Markdown 版面缓存（键含内容、宽度与量算代次）：滚动时长内容不必每帧重排。
    pub markdown_layouts: Arc<markdown::LayoutCache>,
    /// 上次已提交的帧与所在面：同内容帧不再提交（宿主每收一帧都要重投影并整窗重绘）。
    /// 面重新挂载时宿主会再发一次 `Resize`，据此清空——否则重挂后同内容帧被吞，面会停在空态。
    pub submitted: Option<(String, Arc<SurfaceFrame>)>,
    /// 尺寸刚变、下一帧还没交：让 `render` 先按估算字宽交一帧，不再等量算往返。
    pub resize_pending: bool,
    /// 上一帧声明的浮层区域：有浮层时滚轮只作用于浮层内的滚动区。
    pub overlays: Vec<Rect>,
    /// 上一帧的缩放浮层：指针按所在浮层的倍率反算为内容坐标。
    pub scaled_regions: Vec<ScaledRegion>,
    /// 浮层内的滚动容器 id：浮层打开时不把滚轮落到下层列表。
    pub overlay_scrolls: std::collections::HashSet<String>,
    /// 已展开的长代码块（键 = `<Markdown 块 id>:<内容指纹>::code:<序>`）：控件层自持，点击就地翻转。
    pub expanded: std::collections::HashSet<String>,
    /// 虚拟列表各条目的最近一次实测高（列表 id + 条目 key）：只用于估算屏外条目，
    /// 改宽后优先按新宽度重排屏内条目。
    pub virtual_heights: Arc<Mutex<HashMap<(String, u64), f32>>>,
    /// 相邻两块（按绘制序）之间源文里的空行数（插件按自己的文档模型提供）。
    /// 跨块复制按它复原段落间隔；未设置时块间固定按一个换行拼接。
    pub gap_of: Option<Box<dyn Fn(&str, &str) -> usize + Send + Sync>>,
}

/// 相邻两块之间空行数的解析器：入参为两块的控件 id（绘制序前后）。
pub type GapResolver = Box<dyn Fn(&str, &str) -> usize + Send + Sync>;

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
            copy_menu: None,
            markdown_last: HashMap::new(),
            markdown_order: Vec::new(),
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
            scroll_x: HashMap::new(),
            markdown_layouts: Arc::default(),
            gap_of: None,
            submitted: None,
            resize_pending: false,
            overlays: Vec::new(),
            scaled_regions: Vec::new(),
            overlay_scrolls: std::collections::HashSet::new(),
            expanded: std::collections::HashSet::new(),
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

    /// 指针的屏幕坐标 → 内容坐标：落在缩放浮层内时按该层倍率反算（后画的浮层优先）。
    pub(crate) fn base_point(&self, x: f32, y: f32) -> (f32, f32) {
        self.scaled_regions
            .iter()
            .rev()
            .find(|region| region.screen.contains(x, y))
            .map(|region| region.to_base(x, y))
            .unwrap_or((x, y))
    }

    /// 指针位置的输出倍率（不在缩放浮层内为 1.0）：滚轮增量按它换算成内容坐标。
    pub(crate) fn scale_at(&self, x: f32, y: f32) -> f32 {
        self.scaled_regions
            .iter()
            .rev()
            .find(|region| region.screen.contains(x, y))
            .map(|region| region.scale)
            .unwrap_or(1.0)
    }

    /// 收起提示（指针离开面 / 面失焦）。
    pub(crate) fn clear_hint(&mut self) {
        self.hint = None;
        self.pointer = None;
    }

    /// 某 Markdown 块里已展开的代码块序号（排版时判定显示全部行还是折叠）。
    pub(crate) fn expanded_codes(&self, owner: &str) -> Vec<usize> {
        let prefix = format!("{owner}::code:");
        let mut codes: Vec<usize> = self
            .expanded
            .iter()
            .filter_map(|key| key.strip_prefix(&prefix)?.parse().ok())
            .collect();
        codes.sort_unstable();
        codes
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

    /// 复制选择区内的文字（按块序逐块取，不含 Markdown 标记）。块间分隔 = 一个换行 +
    /// 插件解析器给出的块间空行数，跨块复制据此复原段落间隔。
    pub(crate) fn selection_text(&self) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let (from_rank, to_rank, from, to) = self.selection_ends(selection)?;
        let mut out = String::new();
        let mut previous: Option<&str> = None;
        for (rank, id) in self.markdown_order.iter().enumerate() {
            if rank < from_rank || rank > to_rank {
                continue;
            }
            let Some(layout) = self.markdown_last.get(id) else { continue };
            let Some(range) = local_selection_range(layout, rank, from_rank, to_rank, from, to) else { continue };
            let text = markdown::selected_text(layout, range);
            if !text.is_empty() {
                if let Some(prev) = previous {
                    let gaps = self.gap_of.as_ref().map_or(1, |gap_of| 1 + gap_of(prev, id));
                    out.push_str(&"\n".repeat(gaps));
                }
                out.push_str(&text);
                previous = Some(id);
            }
        }
        (!out.trim().is_empty()).then_some(out)
    }

    /// 本块在该次选择中的局部区间（行序，行内字节位）：本块不在选择范围返回 None。
    pub(crate) fn selection_range(&self, layout: &markdown::Layout, id: &str) -> Option<((usize, usize), (usize, usize))> {
        let selection = self.selection.as_ref()?;
        let (from_rank, to_rank, from, to) = self.selection_ends(selection)?;
        let rank = self.markdown_rank(id)?;
        local_selection_range(layout, rank, from_rank, to_rank, from, to)
    }

    fn markdown_rank(&self, id: &str) -> Option<usize> {
        self.markdown_order.iter().position(|block| block == id)
    }

    /// 选择两端的规范序：起点在前；返回（起点块序，终点块序，起点，终点）。
    fn selection_ends<'s>(&self, selection: &'s TextSelection) -> Option<(usize, usize, &'s BlockPos, &'s BlockPos)> {
        let anchor_rank = self.markdown_rank(&selection.anchor.id)?;
        let focus_rank = self.markdown_rank(&selection.focus.id)?;
        let anchor_first = anchor_rank < focus_rank
            || (anchor_rank == focus_rank && (selection.anchor.line, selection.anchor.byte) <= (selection.focus.line, selection.focus.byte));
        if anchor_first {
            Some((anchor_rank, focus_rank, &selection.anchor, &selection.focus))
        } else {
            Some((focus_rank, anchor_rank, &selection.focus, &selection.anchor))
        }
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
        self.scroll_x.retain(|id, _| self.markdown_last.contains_key(id));
        self.expanded.retain(|key| {
            key.rsplit_once("::code:")
                .and_then(|(owner, _)| owner.rsplit_once(':'))
                .is_some_and(|(id, _)| self.markdown_last.contains_key(id))
        });
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
        // 端点块不再出帧（被虚拟滚动裁掉或已删除）时选区一并作废，避免滚回来复活旧选区
        if self.selection.as_ref().is_some_and(|selection| {
            !self.markdown_last.contains_key(&selection.anchor.id)
                || !self.markdown_last.contains_key(&selection.focus.id)
        }) {
            self.selection = None;
        }
    }
}

/// 选择在某一块里的局部区间：起点块取起点到块尾，终点块取块首到终点，中间块整块。
fn local_selection_range(
    layout: &markdown::Layout,
    rank: usize,
    from_rank: usize,
    to_rank: usize,
    from: &BlockPos,
    to: &BlockPos,
) -> Option<((usize, usize), (usize, usize))> {
    if rank < from_rank || rank > to_rank {
        return None;
    }
    let last_line = layout.line_count().saturating_sub(1);
    let last_byte = markdown::line_len(layout, last_line);
    if from_rank == to_rank {
        return Some(((from.line, from.byte), (to.line, to.byte)));
    }
    if rank == from_rank {
        Some(((from.line, from.byte), (last_line, last_byte)))
    } else if rank == to_rank {
        Some(((0, 0), (to.line, to.byte)))
    } else {
        Some(((0, 0), (last_line, last_byte)))
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

    /// 跨块复制按插件解析器给出的块间空行数复原段落间隔。
    #[test]
    fn cross_block_copy_restores_document_gaps() {
        let mut state = UiState::default();
        state.markdown_order = vec!["a".into(), "b".into()];
        let theme = Theme::default();
        let mut metrics = Metrics::default();
        let mut laid = |text: &str| {
            Arc::new(super::super::markdown::layout_expanded(
                &mut metrics,
                &theme,
                "x",
                text,
                &[],
                300.0,
                0,
                true,
                &[],
            ))
        };
        state.markdown_last.insert("a".into(), laid("甲"));
        state.markdown_last.insert("b".into(), laid("乙"));
        state.selection = Some(TextSelection {
            anchor: BlockPos { id: "a".into(), line: 0, byte: 0 },
            focus: BlockPos { id: "b".into(), line: 0, byte: 3 },
        });
        // 未设解析器：块间一个换行
        assert_eq!(state.selection_text().as_deref(), Some("甲\n乙"));
        // 解析器给出两段空行：换行数 = 1 + 空行数
        state.gap_of = Some(Box::new(|_, _| 2));
        assert_eq!(state.selection_text().as_deref(), Some("甲\n\n\n乙"));
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

    /// 缩放浮层把屏幕坐标反算成基础坐标：倍率取所在浮层（后画的优先），浮层外不变。
    #[test]
    fn scaled_region_maps_screen_points_back_to_base() {
        let mut state = UiState::default();
        state.scaled_regions.push(ScaledRegion {
            screen: Rect::new(40.0, 30.0, 200.0, 160.0),
            scale: 2.0,
        });
        assert_eq!(state.base_point(60.0, 50.0), (30.0, 25.0));
        assert_eq!(state.scale_at(60.0, 50.0), 2.0);
        assert_eq!(state.base_point(10.0, 10.0), (10.0, 10.0), "浮层外坐标不变");
        assert_eq!(state.scale_at(10.0, 10.0), 1.0);
    }
}
