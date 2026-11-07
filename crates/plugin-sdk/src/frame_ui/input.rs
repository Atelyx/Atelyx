//! 原始输入 → 语义事件：指针按命中 id 与上次布局解释成点击 / 聚焦，键盘与输入法落到聚焦控件上编辑。

use std::collections::HashMap;
use std::time::Instant;

use atelyx_plugin_protocol::frame::FrameInput;
use atelyx_plugin_protocol::frame::{Key, Modifiers, MouseButton, NamedKey};
use atelyx_plugin_protocol::method::MAX_MEASURE_ITEMS;

use super::geom::Rect;
use super::model::{BlockPos, Drag, FontSpec, InputState, Layout, Press, TextSelection, UiState, COPY_MENU_ID};
use super::text;
use crate::frame_ui::UiEvent;
use crate::Context;

/// 自绘子区起拖的最小位移（逻辑像素）：小于它仍按点击 / 双击解释。
const DRAG_THRESHOLD: f32 = 4.0;

/// 一条输入的处理结果：语义事件，以及这次输入是否改变了画面。
pub struct InputOutcome {
    /// 解释出的语义事件（按发生顺序）。
    pub events: Vec<UiEvent>,
    /// 本次输入是否改变了影响绘制或命中的状态。为 `false` 时插件可以跳过 `render`：
    /// 重排出来的帧与上一帧相同。纯悬停移动（悬停区、焦点、光标、选区、滚动都不变）即此列。
    pub repaint: bool,
}

impl InputOutcome {
    fn unchanged() -> Self {
        Self { events: Vec::new(), repaint: false }
    }
}

/// 影响绘制 / 命中的交互态快照：输入前后相等且没有事件 = 这次输入不必重绘。
/// 只收输入会改的字段；`layouts` / `hints` 这类由 `render` 写回的字段不算。
/// 整表克隆在指针高频事件上仍是微秒级，换来的是「漏一个字段就画面不刷新」的风险为零。
#[derive(PartialEq)]
struct PaintState {
    size: (f32, f32),
    hovered: Option<String>,
    /// 提示可见时指针位置决定提示贴哪画，才需要纳入比较。
    pointer: Option<(f32, f32)>,
    hint: Option<(String, Instant)>,
    press: Option<(String, bool)>,
    drag_bar: Option<String>,
    drag: Option<(String, f32, f32, bool)>,
    focus: Option<String>,
    /// 各输入框的（光标，选区锚点，预输入串，内部滚动偏移）。
    inputs: HashMap<String, (usize, usize, String, f32)>,
    selects: HashMap<String, (bool, Option<usize>)>,
    scroll: HashMap<String, f32>,
    scroll_x: HashMap<String, f32>,
    selection: Option<TextSelection>,
    copy_menu: Option<(f32, f32)>,
}

impl PaintState {
    fn of(state: &UiState) -> Self {
        Self {
            size: state.size,
            hovered: state.hovered.clone(),
            pointer: if state.hint_label().is_some() { state.pointer } else { None },
            hint: state.hint.as_ref().map(|hint| (hint.id.clone(), hint.since)),
            press: state
                .press
                .as_ref()
                .map(|press| (press.id.clone(), press.from_input)),
            drag_bar: state.drag_bar.clone(),
            drag: state
                .drag
                .as_ref()
                .map(|drag| (drag.id.clone(), drag.dx, drag.dy, drag.started)),
            focus: state.focus.clone(),
            inputs: state
                .inputs
                .iter()
                .map(|(id, item)| {
                    (id.clone(), (item.caret, item.anchor, item.preedit.clone(), item.scroll))
                })
                .collect(),
            selects: state
                .selects
                .iter()
                .map(|(id, select)| (id.clone(), (select.open, select.hover)))
                .collect(),
            scroll: state.scroll.clone(),
            scroll_x: state.scroll_x.clone(),
            selection: state.selection.clone(),
            copy_menu: state.copy_menu,
        }
    }
}

/// 一条输入对应的语义事件；改完状态后由插件按 [`InputOutcome::repaint`] 决定是否提交新一帧。
pub(crate) async fn handle(
    state: &mut UiState,
    ctx: &Context,
    input: &FrameInput,
    value_of: impl Fn(&str) -> Option<String>,
) -> InputOutcome {
    let mut events = Vec::new();
    // 按在浮层外：浮层已由宿主收起，控件层不产生事件（插件按原始输入自己收尾）
    if matches!(input, FrameInput::Outside { .. }) {
        return InputOutcome::unchanged();
    }
    let paint_before = PaintState::of(state);
    let caret_was_lit = state.caret_lit();
    let caret_before = caret_snapshot(state);
    match input {
        FrameInput::Outside { .. } => {}
        FrameInput::PointerDown { x, y, button, hit, .. } => {
            if *button == MouseButton::Right {
                // 右键不改焦点与按下态：选区复制菜单自己处理，其余落点报给插件
                right_click(state, *x, *y, hit.as_deref(), &mut events);
            } else {
                let (bx, by) = state.base_point(*x, *y);
                pointer_down(state, ctx, bx, by, *button, hit.as_deref(), &value_of, &mut events).await;
            }
        }
        FrameInput::PointerUp { x, y, hit, .. } => {
            let (bx, by) = state.base_point(*x, *y);
            pointer_up(state, bx, by, hit.as_deref(), &mut events);
        }
        FrameInput::DoubleClick { x, y, button, hit, .. } => {
            if *button == MouseButton::Right {
                // 右键双击按一次右键处理：不改焦点与按下态
                right_click(state, *x, *y, hit.as_deref(), &mut events);
            } else {
                let (bx, by) = state.base_point(*x, *y);
                pointer_down(state, ctx, bx, by, *button, hit.as_deref(), &value_of, &mut events).await;
                if let Some(id) = hit
                    .as_deref()
                    .filter(|id| matches!(state.layouts.get(*id), Some(Layout::Input { .. } | Layout::TextArea { .. })))
                {
                    if let Some(value) = value_of(id) {
                        let at = state.inputs.get(id).map(|item| item.caret).unwrap_or(0);
                        let (start, end) = word_range(&value, at);
                        let item = state.inputs.entry(id.to_string()).or_default();
                        item.caret = end;
                        item.anchor = start;
                    }
                }
                // 双击先按一次按下处理（聚焦 / 置按下态），再单独上报
                if let Some(id) = hit.as_deref() {
                    events.push(UiEvent::DoubleClick { id: id.to_string() });
                }
            }
        }
        FrameInput::PointerMove { x, y, hit, .. } => {
            state.hovered = hit.clone();
            state.pointer = Some((*x, *y));
            // 只有声明了提示的控件才进入等待；换控件即重新计时
            let hinted = hit.as_deref().filter(|id| state.hints.contains_key(*id));
            state.track_hint(hinted);
            let (bx, by) = state.base_point(*x, *y);
            pointer_move(state, ctx, bx, by, hit.as_deref(), &value_of, &mut events).await;
        }
        FrameInput::PointerLeave => {
            state.hovered = None;
            state.clear_hint();
        }
        FrameInput::Wheel { x, y, delta_x, delta_y, .. } => {
            if let Some(event) = wheel(state, *x, *y, *delta_x, *delta_y) {
                events.push(event);
            }
        }
        FrameInput::Key { key, action, modifiers, text } => {
            if *action == atelyx_plugin_protocol::frame::KeyAction::Press {
                key_press(state, ctx, key, *modifiers, text.as_deref(), &value_of, &mut events).await;
            }
        }
        FrameInput::Text { text, composing, .. } => ime_text(state, text, *composing, &value_of, &mut events),
        FrameInput::Focus { focused } => {
            state.press = None;
            state.drag_bar = None;
            state.hovered = None;
            state.clear_hint();
            // 面失去焦点时拖拽不会再收到抬起：已起拖的会话按取消收尾
            if let Some(drag) = state.drag.take().filter(|drag| drag.started) {
                events.push(UiEvent::DragCancel { id: drag.id });
            }
            if !*focused {
                // 宿主收走了面焦点（点了面外）：带走的光标与组合串一并作废
                if let Some(id) = state.focus.take() {
                    if let Some(item) = state.inputs.get_mut(&id) {
                        item.preedit.clear();
                    }
                    events.push(UiEvent::Focus { id, focused: false });
                }
            }
        }
        FrameInput::Resize { width, height } => {
            state.size = (*width, *height);
            // 面重新挂载（或真改尺寸）时宿主手里未必还有上一帧：同内容去重必须从这里失效
            state.submitted = None;
            // 下一帧先按估算字宽交一版，拖动中的尺寸变化不必等量算往返
            state.resize_pending = true;
        }
    }
    // 只有选移动（没改文本）才单独报光标：改了文本的那一条已经带着新值，
    // 插件回读光标即可；但光标离开 `#` 触发段这类场景文本不变，浮层要靠这条收起
    // 控件层自持的复制钮（代码块头的「复制」）：就地写剪贴板，不把这次点击交给插件
    let mut kept = Vec::with_capacity(events.len());
    let mut expanded_changed = false;
    for event in events {
        if let UiEvent::Click { id } = &event {
            if let Some(Layout::Copy { text }) = state.layouts.get(id) {
                let text = text.clone();
                clip_write(ctx, &text).await;
                state.copy_menu = None;
                continue;
            }
            if let Some(Layout::Expand { key }) = state.layouts.get(id) {
                let key = key.clone();
                if !state.expanded.remove(&key) {
                    state.expanded.insert(key);
                }
                expanded_changed = true;
                continue;
            }
        }
        kept.push(event);
    }
    let mut events = kept;
    let text_changed = events.iter().any(|event| matches!(event, UiEvent::Text { .. }));
    let after = caret_snapshot(state);
    // 编辑与光标移动都把闪烁计时清零：按键的瞬间光标一定是亮的
    if text_changed || caret_before.as_ref() != after.as_ref() {
        state.reset_blink();
    }
    if !text_changed {
        if let Some((id, caret, anchor)) = after {
            // 焦点换了另有 Focus 事件，不必再报一次光标
            let changed = caret_before
                .map(|(before_id, before_caret, before_anchor)| {
                    before_id == id && (before_caret != caret || before_anchor != anchor)
                })
                .unwrap_or(false);
            if changed {
                events.insert(0, UiEvent::Caret { id, caret, anchor });
            }
        }
    }
    // 有事件 = 插件要据此改自己的状态，必然重绘；否则只认交互态与光标相位的实际变化
    let repaint = !events.is_empty()
        || PaintState::of(state) != paint_before
        || expanded_changed
        || state.caret_lit() != caret_was_lit;
    InputOutcome { events, repaint }
}

/// 右键路由：markdown 命中区上有非空选区时开出「复制」菜单并吞掉这次右键；
/// 其余落点先收起菜单，再把落点控件报给插件。
fn right_click(state: &mut UiState, x: f32, y: f32, hit: Option<&str>, events: &mut Vec<UiEvent>) {
    let on_markdown = hit.is_some_and(|id| matches!(state.layouts.get(id), Some(Layout::Markdown { .. })));
    if on_markdown && state.selection_text().is_some() {
        state.copy_menu = Some((x, y));
        return;
    }
    state.copy_menu = None;
    if let Some(id) = hit.filter(|id| *id != COPY_MENU_ID) {
        events.push(UiEvent::RightClick { id: id.to_string() });
    }
}

/// 当前聚焦输入框的 (id, 光标, 选区锚点)。
fn caret_snapshot(state: &UiState) -> Option<(String, usize, usize)> {
    let id = state.focus.clone()?;
    let item = state.inputs.get(&id)?;
    Some((id, item.caret, item.anchor))
}

#[allow(clippy::too_many_arguments)]
async fn pointer_down(
    state: &mut UiState,
    ctx: &Context,
    x: f32,
    y: f32,
    button: MouseButton,
    hit: Option<&str>,
    value_of: &impl Fn(&str) -> Option<String>,
    events: &mut Vec<UiEvent>,
) {
    if button != MouseButton::Left {
        return;
    }
    // 点在任何别处都收起选区复制菜单；点菜单项时保留到这次点击处理完
    if state.copy_menu.is_some() && hit != Some(COPY_MENU_ID) {
        state.copy_menu = None;
    }
    // 新的一次按下换掉上一轮拖拽候选：拖拽只在同一次按下内成立
    state.drag = None;
    // 点在别处即放弃上一处选择（本块内的按下会在下面重新起选）；
    // 菜单项复用这段选区文本，按下它时不能把选区清掉，否则重绘后菜单命中区与回执都消失
    let keeps_selection = hit.is_some_and(|id| {
        id == COPY_MENU_ID || matches!(state.layouts.get(id), Some(Layout::Markdown { .. }))
    });
    if !keeps_selection {
        state.selection = None;
    }
    let Some(id) = hit else {
        close_selects(state, None);
        set_focus(state, None, value_of, events);
        state.press = None;
        return;
    };
    let Some(layout) = state.layouts.get(id).cloned() else {
        close_selects(state, None);
        state.press = None;
        return;
    };
    // 点在任何别处都收起展开的下拉（点自己的触发钮或它的选项行除外）
    let keep = match &layout {
        Layout::Select { .. } => Some(id),
        Layout::Option { select, .. } => Some(select.as_str()),
        _ => None,
    };
    close_selects(state, keep);
    // 输入法组合期点选会打断组合会话：焦点与光标原样保留，这次按下不接管
    if matches!(layout, Layout::Input { .. } | Layout::TextArea { .. })
        && state.focus.as_deref() == Some(id)
        && state.inputs.get(id).is_some_and(|item| !item.preedit.is_empty())
    {
        state.press = None;
        return;
    }
    match layout {
        Layout::Input { inner, font, value: shown, .. } => {
            set_focus(state, Some(id.to_string()), value_of, events);
            let current = value_of(id).unwrap_or(shown);
            let caret = caret_at(state, ctx, inner, font, &current, x).await;
            let item = state.inputs.entry(id.to_string()).or_default();
            item.caret = caret;
            item.anchor = caret;
            item.preedit.clear();
            state.press = Some(Press { id: id.to_string(), from_input: true });
        }
        Layout::TextArea { inner, font, value: shown, marks, line, .. } => {
            // 点回正文恢复光标跟随：人工滚动位并回输入态
            if let Some(offset) = state.scroll.remove(id) {
                if let Some(item) = state.inputs.get_mut(id) {
                    item.scroll = offset;
                }
            }
            // 可视首行取决于落点前的光标所在行：先按旧光标换算，再改光标与焦点
            let current = value_of(id).unwrap_or(shown);
            let was_focused = state.focus.as_deref() == Some(id);
            let old = state.inputs.get(id).map(|item| item.caret).unwrap_or(0);
            let caret =
                area_caret_at(state, ctx, id, inner, font, line, &current, x, y, old, was_focused).await;
            // 点在标记（`#` 引用标签这类整段实体）内部 = 整段选中，之后一个退格删掉整条
            let (caret, anchor) = match mark_at(&marks, caret) {
                Some((start, end)) => (end, start),
                None => (caret, caret),
            };
            let scroll = state.inputs.get(id).map(|item| item.scroll).unwrap_or(0.0);
            // 只改光标/选区与滚动，不整条替换输入态：default() 会把撤销/重做历史一并清掉
            let item = state.inputs.entry(id.to_string()).or_default();
            item.caret = caret;
            item.anchor = anchor;
            item.scroll = scroll;
            item.preedit.clear();
            set_focus(state, Some(id.to_string()), value_of, events);
            state.press = Some(Press { id: id.to_string(), from_input: true });
        }
        Layout::Select { .. } => {
            set_focus(state, Some(id.to_string()), value_of, events);
            let select = state.selects.entry(id.to_string()).or_default();
            select.open = !select.open;
            select.hover = None;
            state.press = Some(Press { id: id.to_string(), from_input: false });
        }
        Layout::Checkbox => {
            set_focus(state, Some(id.to_string()), value_of, events);
            state.press = Some(Press { id: id.to_string(), from_input: false });
        }
        Layout::Button
        | Layout::Option { .. }
        | Layout::Copy { .. }
        | Layout::Expand { .. }
        | Layout::Link { .. } => {
            state.press = Some(Press { id: id.to_string(), from_input: false });
        }
        Layout::Markdown { rect, .. } => {
            // 只读正文里按下即起选：拖过多少选多少，同一次按下内可跨块延伸
            let at = markdown_point(state, id, rect, x, y).unwrap_or((0, 0));
            let pos = BlockPos { id: id.to_string(), line: at.0, byte: at.1 };
            state.selection = Some(TextSelection { anchor: pos.clone(), focus: pos });
            state.press = Some(Press { id: id.to_string(), from_input: true });
        }
        Layout::Custom => {
            state.press = Some(Press { id: id.to_string(), from_input: false });
            // 只有光标声明为拖动形态的子区才吃拖拽；普通点选区的小抖动仍按点击解释
            if state.draggables.contains(id) {
                state.drag = Some(Drag {
                    id: id.to_string(),
                    origin: (x, y),
                    dx: 0.0,
                    dy: 0.0,
                    started: false,
                });
            }
        }
        Layout::Scroll { .. } => state.press = None,
        Layout::ScrollBar { scroll, track, thumb, max_offset } => {
            // 滚动条按下即可跳转，并进入拖动：移动继续按落点改偏移
            state.press = None;
            state.drag_bar = Some(id.to_string());
            let offset = bar_offset(track, thumb, max_offset, y);
            set_bar_offset(state, &scroll, offset, max_offset, events);
        }
        Layout::ScrollBarX { owner, track, thumb, max_offset } => {
            state.press = None;
            state.drag_bar = Some(id.to_string());
            let offset = bar_offset_x(track, thumb, max_offset, x);
            if (state.scroll_x.get(&owner).copied().unwrap_or(0.0) - offset).abs() >= 0.01 {
                state.scroll_x.insert(owner.clone(), offset);
                events.push(UiEvent::Scroll { id: owner, offset, max_offset });
            }
        }
    }
}

/// 只读正文里的落点 → （行序，行内字节位）：坐标先换算到块本地（含横向滚动偏移）再按版面命中。
fn markdown_point(state: &mut UiState, id: &str, rect: Rect, x: f32, y: f32) -> Option<(usize, usize)> {
    let layout = state.markdown_last.get(id)?.clone();
    // 与绘制同一上限：窄于内容自然宽时偏移才生效，避免把越界偏移算进落点
    let max_offset = (layout.content_width() - rect.w).max(0.0);
    let offset = state.scroll_x.get(id).copied().unwrap_or(0.0).clamp(0.0, max_offset);
    super::markdown::hit(&layout, &mut state.metrics, x - rect.x + offset, y - rect.y)
}

/// 指针落在哪个 Markdown 块：优先当前命中 id，落在块间空白时取纵向最近的块。
fn markdown_under(state: &UiState, hit: Option<&str>, x: f32, y: f32) -> Option<(String, Rect)> {
    if let Some(id) = hit {
        if let Some(Layout::Markdown { rect, .. }) = state.layouts.get(id) {
            return Some((id.to_string(), *rect));
        }
    }
    let mut best: Option<(String, Rect, f32)> = None;
    for (id, layout) in &state.layouts {
        let Layout::Markdown { rect, .. } = layout else { continue };
        let gap = if y < rect.y {
            rect.y - y
        } else if y > rect.bottom() {
            y - rect.bottom()
        } else if x < rect.x {
            rect.x - x
        } else if x > rect.right() {
            x - rect.right()
        } else {
            0.0
        };
        if best.as_ref().is_none_or(|(_, _, nearest)| gap < *nearest) {
            best = Some((id.clone(), *rect, gap));
        }
    }
    best.map(|(id, rect, _)| (id, rect))
}

fn pointer_up(state: &mut UiState, x: f32, y: f32, hit: Option<&str>, events: &mut Vec<UiEvent>) {
    state.drag_bar = None;
    // 已起拖的会话以抬起收尾：插件据累计位移落结果，这次抬起不再算点击
    if let Some(drag) = state.drag.take() {
        if drag.started {
            state.press = None;
            events.push(UiEvent::DragEnd {
                id: drag.id,
                dx: x - drag.origin.0,
                dy: y - drag.origin.1,
            });
            return;
        }
    }
    let Some(press) = state.press.take() else {
        return;
    };
    if hit != Some(press.id.as_str()) {
        return;
    }
    enum Outcome {
        Click,
        Select { select: String, index: usize },
        Nothing,
    }
    let outcome = match state.layouts.get(&press.id) {
        Some(Layout::Button)
        | Some(Layout::Checkbox)
        | Some(Layout::Custom)
        | Some(Layout::Copy { .. })
        | Some(Layout::Expand { .. })
        | Some(Layout::Link { .. }) => Outcome::Click,
        Some(Layout::Option { select, index, .. }) => Outcome::Select { select: select.clone(), index: *index },
        _ => Outcome::Nothing,
    };
    match outcome {
        Outcome::Click => events.push(UiEvent::Click { id: press.id }),
        Outcome::Select { select, index } => {
            if let Some(state) = state.selects.get_mut(&select) {
                state.open = false;
            }
            events.push(UiEvent::Select { id: select, index });
        }
        Outcome::Nothing => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn pointer_move(
    state: &mut UiState,
    ctx: &Context,
    x: f32,
    y: f32,
    hit: Option<&str>,
    value_of: &impl Fn(&str) -> Option<String>,
    events: &mut Vec<UiEvent>,
) {
    if let Some(bar_id) = state.drag_bar.clone() {
        match state.layouts.get(&bar_id).cloned() {
            Some(Layout::ScrollBar { scroll, track, thumb, max_offset }) => {
                let offset = bar_offset(track, thumb, max_offset, y);
                if (state.scroll.get(&scroll).copied().unwrap_or(0.0) - offset).abs() >= 0.01 {
                    set_bar_offset(state, &scroll, offset, max_offset, events);
                }
                return;
            }
            Some(Layout::ScrollBarX { owner, track, thumb, max_offset }) => {
                let offset = bar_offset_x(track, thumb, max_offset, x);
                if (state.scroll_x.get(&owner).copied().unwrap_or(0.0) - offset).abs() >= 0.01 {
                    state.scroll_x.insert(owner.clone(), offset);
                    events.push(UiEvent::Scroll { id: owner, offset, max_offset });
                }
                return;
            }
            _ => state.drag_bar = None,
        }
    }
    // 自绘子区的拖拽不看落点命中：手指常常拖出子区，位移才是语义
    if let Some(mut drag) = state.drag.take() {
        drag.dx = x - drag.origin.0;
        drag.dy = y - drag.origin.1;
        if !drag.started && drag.dx.abs().max(drag.dy.abs()) >= DRAG_THRESHOLD {
            drag.started = true;
            events.push(UiEvent::DragStart { id: drag.id.clone() });
        }
        if drag.started {
            events.push(UiEvent::Drag { id: drag.id.clone(), dx: drag.dx, dy: drag.dy });
        }
        state.drag = Some(drag);
        return;
    }
    // 只读正文里按住拖动：落点换算成行列，跨块时按块序延伸当前选择
    if let Some(press) = state.press.clone().filter(|press| press.from_input) {
        if matches!(state.layouts.get(&press.id), Some(Layout::Markdown { .. })) {
            if let Some((target_id, rect)) = markdown_under(state, hit, x, y) {
                if let Some(at) = markdown_point(state, &target_id, rect, x, y) {
                    let pos = BlockPos { id: target_id, line: at.0, byte: at.1 };
                    if let Some(selection) = state.selection.as_mut() {
                        selection.focus = pos;
                    }
                }
            }
            return;
        }
    }
    let Some(press) = state.press.clone().filter(|press| press.from_input) else {
        return;
    };
    if hit != Some(press.id.as_str()) {
        return;
    }
    let Some(layout) = state.layouts.get(&press.id).cloned() else {
        return;
    };
    let caret = match layout {
        Layout::Input { inner, font, value: shown } => {
            let current = value_of(&press.id).unwrap_or(shown);
            caret_at(state, ctx, inner, font, &current, x).await
        }
        Layout::TextArea { inner, font, value: shown, line, .. } => {
            let current = value_of(&press.id).unwrap_or(shown);
            let old = state.inputs.get(&press.id).map(|item| item.caret).unwrap_or(0);
            area_caret_at(state, ctx, &press.id, inner, font, line, &current, x, y, old, true).await
        }
        _ => return,
    };
    if let Some(item) = state.inputs.get_mut(&press.id) {
        item.caret = caret;
    }
}

/// 滚动条落点 → 偏移：按滑块中心随指针走，钳到可滚动区间。
fn bar_offset(track: Rect, thumb: f32, max_offset: f32, y: f32) -> f32 {
    let travel = (track.h - thumb).max(1.0);
    let ratio = ((y - track.y - thumb / 2.0) / travel).clamp(0.0, 1.0);
    ratio * max_offset
}

/// 滚动条落点写回目标：普通容器记在 `scroll`，多行输入框同记在 `scroll` 覆盖位（暂停光标跟随）
/// 并同步到输入态（落点换算与光标矩形读输入态）；输入框的滚动不给插件发 Scroll 事件。
fn set_bar_offset(
    state: &mut UiState,
    target: &str,
    offset: f32,
    max_offset: f32,
    events: &mut Vec<UiEvent>,
) {
    state.scroll.insert(target.to_string(), offset);
    if let Some(Layout::TextArea { .. }) = state.layouts.get(target) {
        if let Some(item) = state.inputs.get_mut(target) {
            item.scroll = offset;
        }
        return;
    }
    events.push(UiEvent::Scroll { id: target.to_string(), offset, max_offset });
}

/// 横向滚动条落点 → 偏移。
fn bar_offset_x(track: Rect, thumb: f32, max_offset: f32, x: f32) -> f32 {
    let travel = (track.w - thumb).max(1.0);
    let ratio = ((x - track.x - thumb / 2.0) / travel).clamp(0.0, 1.0);
    ratio * max_offset
}

/// 滚轮改滚动偏移：增量是像素（宿主已把「行」换算成像素），按光标所在的最内层滚动容器走。
/// 落点与增量都按所在缩放浮层的倍率换算回基础坐标：滚动偏移以基础单位存储，跨缩放连续。
/// 增量是屏幕像素，除倍率才是基础单位；乘倍率会让滚速随缩放平方变化。
fn wheel(state: &mut UiState, x: f32, y: f32, delta_x: f32, delta_y: f32) -> Option<UiEvent> {
    // 浮层打开时滚轮只作用于浮层内：落点不在浮层上即丢弃，也不把滚动漏给下层列表
    if !state.overlays.is_empty() && !state.overlays.iter().any(|rect| rect.contains(x, y)) {
        return None;
    }
    let scale = state.scale_at(x, y);
    let (x, y) = state.base_point(x, y);
    let (delta_x, delta_y) = (delta_x / scale, delta_y / scale);
    // 光标落在内容超出一屏的多行输入框上：滚轮先滚输入框内部（与原生输入面一致）
    let area = state
        .layouts
        .iter()
        .filter_map(|(id, layout)| match layout {
            Layout::TextArea { inner, font, value, line, .. } if inner.contains(x, y) => {
                Some((id.clone(), *inner, *font, value.clone(), *line))
            }
            _ => None,
        })
        .min_by(|a, b| (a.1.w * a.1.h).total_cmp(&(b.1.w * b.1.h)));
    if let Some((id, inner, font, value, line)) = area {
        let content = text::wrapped_height_with_line(&mut state.metrics, &value, font, inner.w, line);
        let max_offset = (content - inner.h).max(0.0);
        if max_offset > 0.5 {
            let item = state.inputs.entry(id.clone()).or_default();
            item.scroll = (item.scroll + delta_y).clamp(0.0, max_offset);
            let offset = item.scroll;
            // 滚轮也是人工滚动：写覆盖位，否则聚焦时会被光标跟随拉回
            state.scroll.insert(id, offset);
            return None;
        }
    }
    // 光标落在块内超宽的 Markdown（表格 / 代码块）上：横向增量先滚块内，不滚整篇
    if delta_x.abs() > 0.01 {
        let target = state
            .layouts
            .iter()
            .filter_map(|(id, layout)| match layout {
                Layout::Markdown { rect, content_w } if rect.contains(x, y) && *content_w > rect.w + 0.5 => {
                    Some((id.clone(), *rect, *content_w))
                }
                _ => None,
            })
            .min_by(|a, b| (a.1.w * a.1.h).total_cmp(&(b.1.w * b.1.h)));
        if let Some((id, rect, content_w)) = target {
            let max_offset = (content_w - rect.w).max(0.0);
            let current = state.scroll_x.get(&id).copied().unwrap_or(0.0);
            let next = (current + delta_x).clamp(0.0, max_offset);
            if (next - current).abs() >= 0.01 {
                state.scroll_x.insert(id.clone(), next);
                return Some(UiEvent::Scroll { id, offset: next, max_offset });
            }
        }
    }

    let (id, content, viewport) = state
        .layouts
        .iter()
        .filter_map(|(id, layout)| match layout {
            Layout::Scroll { viewport, content }
                if viewport.contains(x, y)
                    && (state.overlays.is_empty() || state.overlay_scrolls.contains(id)) =>
            {
                Some((id.clone(), *content, *viewport))
            }
            _ => None,
        })
        .min_by(|a, b| (a.2.w * a.2.h).total_cmp(&(b.2.w * b.2.h)))?;
    let max_offset = (content - viewport.h).max(0.0);
    let current = state.scroll.get(&id).copied().unwrap_or(0.0);
    // 滚动位可能大于 max（改宽后内容变矮，保留原内容位）：以可视位置为基准滚，
    // 不把超出部分折叠进本次滚动
    let base = current.min(max_offset);
    let next = (base + delta_y).clamp(0.0, max_offset);
    if (next - base).abs() < 0.01 {
        return None;
    }
    state.scroll.insert(id.clone(), next);
    Some(UiEvent::Scroll { id, offset: next, max_offset })
}

async fn key_press(
    state: &mut UiState,
    ctx: &Context,
    key: &Key,
    modifiers: Modifiers,
    text: Option<&str>,
    value_of: &impl Fn(&str) -> Option<String>,
    events: &mut Vec<UiEvent>,
) {
    if let Key::Named { name: NamedKey::Tab } = key {
        move_focus(state, modifiers.shift, value_of, events);
        return;
    }
    if matches!(key, Key::Named { name: NamedKey::Escape }) {
        state.copy_menu = None;
    }
    // 拖拽中的 Esc 先取消拖拽（未起拖的候选只清掉，不影响后续按键语义）
    if matches!(key, Key::Named { name: NamedKey::Escape }) {
        if let Some(drag) = state.drag.take().filter(|drag| drag.started) {
            state.press = None;
            events.push(UiEvent::DragCancel { id: drag.id });
            return;
        }
    }
    // 只读正文的选择：Ctrl+C 直接复制（不需要输入框聚焦）
    if modifiers.control
        && matches!(key, Key::Character { text } if text.eq_ignore_ascii_case("c"))
    {
        if let Some(text) = state.selection_text() {
            clip_write(ctx, &text).await;
            return;
        }
    }
    let Some(id) = state.focus.clone() else {
        return;
    };
    let Some(layout) = state.layouts.get(&id).cloned() else {
        return;
    };
    // 输入法组合期按键归输入法（取词 / 取消 / 翻页）：焦点、光标与文本一律不动。
    // 提交文本随后经 `Text` 到达，取消由输入法的闭合事件收尾。
    if state.inputs.get(&id).is_some_and(|item| !item.preedit.is_empty()) {
        return;
    }
    if let Key::Named { name: NamedKey::Escape } = key {
        match layout {
            Layout::Select { .. } => {
                if let Some(select) = state.selects.get_mut(&id) {
                    select.open = false;
                }
            }
            Layout::Input { .. } | Layout::TextArea { .. } => {
                if let Some(item) = state.inputs.get_mut(&id) {
                    item.preedit.clear();
                }
                // 先报取消再离焦：插件靠这个顺序区分「放弃草稿」与「失焦提交」
                events.push(UiEvent::Cancel { id: id.clone() });
                set_focus(state, None, value_of, events);
            }
            _ => {}
        }
        return;
    }
    // 单行输入的回车是「提交」而非编辑：值由插件回读，语义（检索 / 写回设置）由插件定。
    // 多行输入只有声明了回车提交才有此语义，Shift+回车仍是换行。
    if matches!(key, Key::Named { name: NamedKey::Enter }) {
        let submits = match &layout {
            Layout::Input { .. } => true,
            Layout::TextArea { enter_submits, .. } => *enter_submits && !modifiers.shift,
            _ => false,
        };
        if submits {
            events.push(UiEvent::Submit { id });
            return;
        }
    }
    match layout {
        Layout::Select { options, .. } => select_key(state, &id, options, key, events),
        Layout::Checkbox => {
            if matches!(key, Key::Named { name: NamedKey::Space }) {
                events.push(UiEvent::Click { id });
            }
        }
        Layout::Input { .. } => {
            edit_key(state, ctx, &id, key, modifiers, text, value_of, events, None).await
        }
        Layout::TextArea { inner, font, marks, .. } => {
            let area = AreaCtx { inner, font, marks };
            edit_key(state, ctx, &id, key, modifiers, text, value_of, events, Some(&area)).await
        }
        _ => {}
    }
}

fn select_key(state: &mut UiState, id: &str, options: usize, key: &Key, events: &mut Vec<UiEvent>) {
    let select = state.selects.entry(id.to_string()).or_default();
    if options == 0 {
        return;
    }
    match key {
        Key::Named { name: NamedKey::ArrowDown } => {
            if select.open {
                select.hover = Some(select.hover.map(|index| (index + 1).min(options - 1)).unwrap_or(0));
            } else {
                select.open = true;
                select.hover = Some(0);
            }
        }
        Key::Named { name: NamedKey::ArrowUp } => {
            if select.open {
                select.hover = Some(select.hover.map(|index| index.saturating_sub(1)).unwrap_or(options - 1));
            } else {
                select.open = true;
                select.hover = Some(options - 1);
            }
        }
        Key::Named { name: NamedKey::Enter | NamedKey::Space } => {
            if select.open {
                if let Some(index) = select.hover {
                    select.open = false;
                    select.hover = None;
                    events.push(UiEvent::Select { id: id.to_string(), index });
                } else {
                    select.open = false;
                }
            } else {
                select.open = true;
                select.hover = Some(0);
            }
        }
        Key::Named { name: NamedKey::Escape } => {
            select.open = false;
            select.hover = None;
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn edit_key(
    state: &mut UiState,
    ctx: &Context,
    id: &str,
    key: &Key,
    modifiers: Modifiers,
    text: Option<&str>,
    value_of: &impl Fn(&str) -> Option<String>,
    events: &mut Vec<UiEvent>,
    area: Option<&AreaCtx>,
) {
    let Some(current) = current_value(state, id, value_of) else {
        return;
    };
    // 编辑或移动光标后回到光标跟随：人工滚动位失效
    state.scroll.remove(id);
    if modifiers.control && !modifiers.alt {
        if let Key::Character { text: character } = key {
            match character.to_ascii_lowercase().as_str() {
                "a" => {
                    let item = state.inputs.entry(id.to_string()).or_default();
                    item.anchor = 0;
                    item.caret = current.len();
                }
                "c" => {
                    if let Some((from, to)) = selection(&current, state.inputs.get(id)) {
                        clip_write(ctx, &current[from..to]).await;
                    }
                }
                "x" => {
                    if let Some((from, to)) = selection(&current, state.inputs.get(id)) {
                        clip_write(ctx, &current[from..to]).await;
                        let item = state.inputs.entry(id.to_string()).or_default();
                        // 剪切与粘贴同口径入撤销栈：否则一步撤销会跳过剪切态直接回到更早的快照
                        push_history(item, &current);
                        item.caret = from;
                        item.anchor = from;
                        let value = format!("{}{}", &current[..from], &current[to..]);
                        events.push(UiEvent::Text { id: id.to_string(), value });
                    }
                }
                "z" => {
                    if let Some(value) = step_history(state, id, &current, false) {
                        events.push(UiEvent::Text { id: id.to_string(), value });
                    }
                }
                "y" => {
                    if let Some(value) = step_history(state, id, &current, true) {
                        events.push(UiEvent::Text { id: id.to_string(), value });
                    }
                }
                "v" => match ctx.clipboard_read().await {
                    Ok(text) => {
                        if let Some(value) = insert(state, id, &current, &text) {
                            events.push(UiEvent::Text { id: id.to_string(), value });
                        }
                    }
                    Err(err) => ctx.log_error(&format!("读取剪贴板失败：{err}")),
                },
                _ => {}
            }
        }
        // Ctrl+Home / Ctrl+End / PageUp / PageDown 是光标跳转，继续往下交给多行框处理
        if !matches!(
            key,
            Key::Named { name: NamedKey::Home | NamedKey::End | NamedKey::PageUp | NamedKey::PageDown }
        ) {
            return;
        }
    }
    if let Some(area) = area {
        let (inner, font) = (area.inner, area.font);
        // 多行的上下键与行首尾按可视行走：按整篇首尾跳会跨过一整段
        if let Key::Named { name: NamedKey::ArrowUp | NamedKey::ArrowDown } = key {
            let up = matches!(key, Key::Named { name: NamedKey::ArrowUp });
            move_vertical(state, ctx, id, &current, inner, font, up, modifiers.shift).await;
            return;
        }
        if let Key::Named { name: NamedKey::Home | NamedKey::End } = key {
            let end = matches!(key, Key::Named { name: NamedKey::End });
            if modifiers.control {
                // Ctrl+Home / Ctrl+End：文档首尾（与可视行首尾区分）
                refill(state, ctx, &current, font, inner.w).await;
                let item = state.inputs.entry(id.to_string()).or_default();
                item.caret = if end { current.len() } else { 0 };
                if !modifiers.shift {
                    item.anchor = item.caret;
                }
            } else {
                move_line_edge(state, ctx, id, &current, inner, font, end, modifiers.shift).await;
            }
            return;
        }
        if let Key::Named { name: NamedKey::PageUp | NamedKey::PageDown } = key {
            // 按可见高度步进：视口能放下几行就跨几行
            let up = matches!(key, Key::Named { name: NamedKey::PageUp });
            refill(state, ctx, &current, font, inner.w).await;
            let line = text::line_height(&mut state.metrics, font);
            let step = ((inner.h / line).floor() as usize).max(1);
            for _ in 0..step {
                move_vertical(state, ctx, id, &current, inner, font, up, modifiers.shift).await;
            }
            return;
        }
        // 标记边界上的退格 / 删除 = 整段删掉（与整段选中的交互对齐，逐字拆标签没有意义）
        if let Key::Named { name: NamedKey::Backspace | NamedKey::Delete } = key {
            let item = state.inputs.get(id);
            let caret = item.map_or(0, |item| item.caret.min(current.len()));
            let anchor = item.map_or(0, |item| item.anchor.min(current.len()));
            let target = match key {
                Key::Named { name: NamedKey::Backspace } => {
                    mark_ending_at(&area.marks, caret).filter(|_| caret == anchor)
                }
                _ => mark_starting_at(&area.marks, caret).filter(|_| caret == anchor),
            };
            if let Some((from, to)) = target {
                if let Some(value) = apply_edit(state, id, &current, Edit::Cut { from, to }) {
                    events.push(UiEvent::Text { id: id.to_string(), value });
                }
                return;
            }
        }
    }
    let edit = match key {
        Key::Character { .. } => text.map(|text| Edit::Insert(text.to_string())),
        Key::Named { name } => match name {
            NamedKey::Backspace => Some(Edit::Backspace),
            NamedKey::Delete => Some(Edit::Delete),
            NamedKey::Enter => area.map(|_| Edit::Insert("\n".to_string())),
            NamedKey::ArrowLeft => Some(Edit::Left {
                extend: modifiers.shift,
                word: modifiers.control,
            }),
            NamedKey::ArrowRight => Some(Edit::Right {
                extend: modifiers.shift,
                word: modifiers.control,
            }),
            NamedKey::Home => Some(Edit::Home { extend: modifiers.shift }),
            NamedKey::End => Some(Edit::End { extend: modifiers.shift }),
            NamedKey::Space => Some(Edit::Insert(" ".to_string())),
            _ => None,
        },
    };
    if let Some(edit) = edit {
        if let Some(value) = apply_edit(state, id, &current, edit) {
            events.push(UiEvent::Text { id: id.to_string(), value });
        }
    }
}

fn ime_text(
    state: &mut UiState,
    text: &str,
    composing: bool,
    value_of: &impl Fn(&str) -> Option<String>,
    events: &mut Vec<UiEvent>,
) {
    let Some(id) = state.focus.clone() else {
        return;
    };
    if !matches!(state.layouts.get(&id), Some(Layout::Input { .. } | Layout::TextArea { .. })) {
        return;
    }
    if composing {
        let item = state.inputs.entry(id).or_default();
        item.preedit = text.to_string();
        // 组合串每变一次都让光标重新显形：正在打字时不该看到光标熄灭
        state.reset_blink();
        return;
    }
    let Some(current) = current_value(state, &id, value_of) else {
        return;
    };
    if let Some(item) = state.inputs.get_mut(&id) {
        item.preedit.clear();
    }
    state.reset_blink();
    if text.is_empty() {
        return;
    }
    if let Some(value) = insert(state, &id, &current, text) {
        events.push(UiEvent::Text { id, value });
    }
}

/// 多行输入在编辑键上要用的布局信息：可视区与标记区间。
struct AreaCtx {
    inner: Rect,
    font: FontSpec,
    marks: Vec<(usize, usize)>,
}

/// 光标落在哪个标记内部（严格在两端之间）。
fn mark_at(marks: &[(usize, usize)], caret: usize) -> Option<(usize, usize)> {
    marks.iter().copied().find(|(start, end)| caret > *start && caret < *end)
}

/// 光标恰在标记末尾（退格整段删）。
fn mark_ending_at(marks: &[(usize, usize)], caret: usize) -> Option<(usize, usize)> {
    marks.iter().copied().find(|(_, end)| *end == caret)
}

/// 光标恰在标记开头（Delete 整段删）。
fn mark_starting_at(marks: &[(usize, usize)], caret: usize) -> Option<(usize, usize)> {
    marks.iter().copied().find(|(start, _)| *start == caret)
}

enum Edit {
    Insert(String),
    Backspace,
    Delete,
    /// 直接切除区间（标记整段删除用）。
    Cut { from: usize, to: usize },
    /// `word` = 按词跳（Ctrl+左右）。
    Left { extend: bool, word: bool },
    Right { extend: bool, word: bool },
    Home { extend: bool },
    End { extend: bool },
}

fn apply_edit(state: &mut UiState, id: &str, value: &str, edit: Edit) -> Option<String> {
    let item = state.inputs.entry(id.to_string()).or_default();
    let caret = boundary(value, item.caret.min(value.len()));
    let anchor = boundary(value, item.anchor.min(value.len()));
    match edit {
        Edit::Insert(text) => insert(state, id, value, &text),
        Edit::Cut { from, to } => {
            let from = boundary(value, from.min(value.len()));
            let to = boundary(value, to.min(value.len()));
            if from == to {
                return None;
            }
            let item = state.inputs.entry(id.to_string()).or_default();
            push_history(item, value);
            item.caret = from;
            item.anchor = from;
            Some(format!("{}{}", &value[..from], &value[to..]))
        }
        Edit::Backspace => {
            let (from, to) = if caret != anchor { (caret.min(anchor), caret.max(anchor)) } else {
                (prev_boundary(value, caret), caret)
            };
            if from == to {
                return None;
            }
            let item = state.inputs.entry(id.to_string()).or_default();
            push_history(item, value);
            item.caret = from;
            item.anchor = from;
            Some(format!("{}{}", &value[..from], &value[to..]))
        }
        Edit::Delete => {
            let (from, to) = if caret != anchor { (caret.min(anchor), caret.max(anchor)) } else {
                (caret, next_boundary(value, caret))
            };
            if from == to {
                return None;
            }
            let item = state.inputs.entry(id.to_string()).or_default();
            push_history(item, value);
            item.caret = from;
            item.anchor = from;
            Some(format!("{}{}", &value[..from], &value[to..]))
        }
        Edit::Left { extend, word } => {
            let item = state.inputs.entry(id.to_string()).or_default();
            if !extend && caret != anchor {
                item.caret = caret.min(anchor);
                item.anchor = item.caret;
            } else {
                item.caret = if word {
                    word_edge(value, caret, true)
                } else {
                    prev_boundary(value, caret)
                };
                if !extend {
                    item.anchor = item.caret;
                }
            }
            None
        }
        Edit::Right { extend, word } => {
            let item = state.inputs.entry(id.to_string()).or_default();
            if !extend && caret != anchor {
                item.caret = caret.max(anchor);
                item.anchor = item.caret;
            } else {
                item.caret = if word {
                    word_edge(value, caret, false)
                } else {
                    next_boundary(value, caret)
                };
                if !extend {
                    item.anchor = item.caret;
                }
            }
            None
        }
        Edit::Home { extend } => {
            let item = state.inputs.entry(id.to_string()).or_default();
            item.caret = 0;
            if !extend {
                item.anchor = 0;
            }
            None
        }
        Edit::End { extend } => {
            let item = state.inputs.entry(id.to_string()).or_default();
            item.caret = value.len();
            if !extend {
                item.anchor = value.len();
            }
            None
        }
    }
}

/// 插入文本：有选区先替换，光标落到插入内容之后。
fn insert(state: &mut UiState, id: &str, value: &str, text: &str) -> Option<String> {
    let item = state.inputs.entry(id.to_string()).or_default();
    let caret = boundary(value, item.caret.min(value.len()));
    let anchor = boundary(value, item.anchor.min(value.len()));
    let (from, to) = (caret.min(anchor), caret.max(anchor));
    if text.is_empty() && from == to {
        return None;
    }
    push_history(item, value);
    let mut next = String::with_capacity(value.len() + text.len());
    next.push_str(&value[..from]);
    next.push_str(text);
    next.push_str(&value[to..]);
    let position = from + text.len();
    item.caret = position;
    item.anchor = position;
    item.preedit.clear();
    (next != value).then_some(next)
}

fn current_value(state: &UiState, id: &str, value_of: &impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(value) = value_of(id) {
        return Some(value);
    }
    match state.layouts.get(id) {
        Some(Layout::Input { value, .. } | Layout::TextArea { value, .. }) => Some(value.clone()),
        _ => None,
    }
}

/// 撤销栈深度上限：满即淘汰最旧（全量快照，再深也只是内存）。
const UNDO_DEPTH: usize = 200;

/// 记一笔撤销快照（文本变了才记），并作废重做栈。
fn push_history(item: &mut InputState, value: &str) {
    item.undo.push((value.to_string(), item.caret));
    if item.undo.len() > UNDO_DEPTH {
        item.undo.remove(0);
    }
    item.redo.clear();
}

/// 撤销 / 重做一步：返回要写回的新值（无可撤销时返回 `None`）。
fn step_history(state: &mut UiState, id: &str, value: &str, redo: bool) -> Option<String> {
    let item = state.inputs.entry(id.to_string()).or_default();
    let from = if redo { item.redo.pop() } else { item.undo.pop() }?;
    let back = (value.to_string(), item.caret);
    if redo {
        item.undo.push(back);
    } else {
        item.redo.push(back);
    }
    let (value, caret) = from;
    item.caret = boundary(&value, caret.min(value.len()));
    item.anchor = item.caret;
    Some(value)
}

/// 按词跳：`backward` 先跳过空白再回退到词首，否则前进到词尾。
fn word_edge(text: &str, caret: usize, backward: bool) -> usize {
    let mut i = caret.min(text.len());
    while !text.is_char_boundary(i) {
        i = if backward { i.saturating_sub(1) } else { i + 1 };
    }
    if backward {
        while let Some((at, ch)) = text[..i].char_indices().next_back() {
            if ch.is_whitespace() {
                i = at;
            } else {
                break;
            }
        }
        while let Some((at, ch)) = text[..i].char_indices().next_back() {
            if !ch.is_whitespace() {
                i = at;
            } else {
                break;
            }
        }
    } else {
        while let Some(ch) = text[i..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            i += ch.len_utf8();
        }
        while let Some(ch) = text[i..].chars().next() {
            if ch.is_whitespace() {
                break;
            }
            i += ch.len_utf8();
        }
    }
    i
}

/// 双击选词：命中位在词内或词边界取相邻词（右侧优先），空白段取该空白段。
fn word_range(text: &str, caret: usize) -> (usize, usize) {
    let mut at = caret.min(text.len());
    while !text.is_char_boundary(at) {
        at = at.saturating_sub(1);
    }
    let forward_word = text[at..].chars().next().is_some_and(|ch| !ch.is_whitespace());
    let backward_word = text[..at].chars().next_back().is_some_and(|ch| !ch.is_whitespace());
    let mut start = at;
    let mut end = at;
    if !forward_word && !backward_word {
        while let Some(ch) = text[end..].chars().next().filter(|ch| ch.is_whitespace()) {
            end += ch.len_utf8();
        }
        while let Some((before, ch)) = text[..start].char_indices().next_back() {
            if ch.is_whitespace() {
                start = before;
            } else {
                break;
            }
        }
        return (start, end);
    }
    if forward_word {
        while let Some(ch) = text[end..].chars().next().filter(|ch| !ch.is_whitespace()) {
            end += ch.len_utf8();
        }
    }
    if backward_word {
        while let Some((before, ch)) = text[..start].char_indices().next_back() {
            if !ch.is_whitespace() {
                start = before;
            } else {
                break;
            }
        }
    }
    (start, end)
}

/// 选区字节范围（无选区或越界时为空）。
fn selection(value: &str, state: Option<&InputState>) -> Option<(usize, usize)> {
    let item = state?;
    let caret = boundary(value, item.caret.min(value.len()));
    let anchor = boundary(value, item.anchor.min(value.len()));
    (caret != anchor).then(|| (caret.min(anchor), caret.max(anchor)))
}

/// 先把值按可用宽度排一遍，补齐缺的量算：光标落点与选区延伸要按真实字宽换算。
async fn refill(state: &mut UiState, ctx: &Context, value: &str, font: FontSpec, width: f32) {
    text::lines(&mut state.metrics, value, font, width);
    if !state.metrics.has_misses() {
        return;
    }
    let misses = state.metrics.take_misses();
    for chunk in misses.chunks(MAX_MEASURE_ITEMS) {
        match ctx.measure_batch(chunk.to_vec()).await {
            Ok(result) => state.metrics.apply(chunk, &result.sizes),
            Err(err) => {
                ctx.log_error(&format!("量算失败，按估算字宽落光标：{err}"));
                break;
            }
        }
    }
}

async fn caret_at(
    state: &mut UiState,
    ctx: &Context,
    inner: Rect,
    font: FontSpec,
    value: &str,
    x: f32,
) -> usize {
    refill(state, ctx, value, font, inner.w).await;
    let widths = text::prefix_widths(&mut state.metrics, value, font);
    let points = text::boundaries(value);
    let target = (x - inner.x).max(0.0);
    let mut best = 0usize;
    for (index, width) in widths.iter().enumerate() {
        if (*width - target).abs() < (widths[best] - target).abs() {
            best = index;
        }
    }
    points[best]
}

/// 多行输入的落点换算：纵向偏移与绘制同一口径（用户滚动值 + 光标跟随，整行吸附）。
#[allow(clippy::too_many_arguments)]
async fn area_caret_at(
    state: &mut UiState,
    ctx: &Context,
    id: &str,
    inner: Rect,
    font: FontSpec,
    line_height: f32,
    value: &str,
    x: f32,
    y: f32,
    old_caret: usize,
    focused: bool,
) -> usize {
    refill(state, ctx, value, font, inner.w).await;
    let lines = text::lines(&mut state.metrics, value, font, inner.w);
    let stored = state.inputs.get(id).map(|item| item.scroll).unwrap_or(0.0);
    let scroll = text::area_scroll(
        stored,
        text::line_of(&lines, text::snap(value, old_caret)),
        line_height,
        inner.h,
        lines.len(),
        focused,
    );
    let top = (scroll / line_height).round() as usize;
    let row = ((y - inner.y).max(0.0) / line_height) as usize;
    let line = lines[(top + row).min(lines.len().saturating_sub(1))];
    let offset = text::caret_at_x(&mut state.metrics, &value[line.start..line.end], font, (x - inner.x).max(0.0));
    line.start + offset
}

/// 上下键跨可视行移动：保持横向落点，首行再上移到文首、末行再下移到文末。
#[allow(clippy::too_many_arguments)]
async fn move_vertical(
    state: &mut UiState,
    ctx: &Context,
    id: &str,
    value: &str,
    inner: Rect,
    font: FontSpec,
    up: bool,
    extend: bool,
) {
    refill(state, ctx, value, font, inner.w).await;
    let lines = text::lines(&mut state.metrics, value, font, inner.w);
    let item = state.inputs.entry(id.to_string()).or_default();
    let caret = text::snap(value, item.caret);
    let index = text::line_of(&lines, caret);
    let line = lines[index];
    let x = text::offset_in_line(&mut state.metrics, &value[line.start..line.end], font, caret.saturating_sub(line.start));
    let target = if up {
        index.checked_sub(1)
    } else {
        (index + 1 < lines.len()).then_some(index + 1)
    };
    let Some(target) = target else {
        item.caret = if up { 0 } else { value.len() };
        if !extend {
            item.anchor = item.caret;
        }
        return;
    };
    let target = lines[target];
    let offset = text::caret_at_x(&mut state.metrics, &value[target.start..target.end], font, x);
    item.caret = target.start + offset;
    if !extend {
        item.anchor = item.caret;
    }
}

/// 行首 / 行尾键：落在光标所在可视行的两端，而不是整篇首尾。
#[allow(clippy::too_many_arguments)]
async fn move_line_edge(
    state: &mut UiState,
    ctx: &Context,
    id: &str,
    value: &str,
    inner: Rect,
    font: FontSpec,
    end: bool,
    extend: bool,
) {
    refill(state, ctx, value, font, inner.w).await;
    let lines = text::lines(&mut state.metrics, value, font, inner.w);
    let item = state.inputs.entry(id.to_string()).or_default();
    let index = text::line_of(&lines, text::snap(value, item.caret));
    item.caret = if end { lines[index].end } else { lines[index].start };
    if !extend {
        item.anchor = item.caret;
    }
}

fn move_focus(
    state: &mut UiState,
    backward: bool,
    value_of: &impl Fn(&str) -> Option<String>,
    events: &mut Vec<UiEvent>,
) {
    if state.focusables.is_empty() {
        return;
    }
    let count = state.focusables.len();
    let current = state
        .focus
        .as_ref()
        .and_then(|id| state.focusables.iter().position(|item| item == id));
    let next = match current {
        Some(index) if backward => (index + count - 1) % count,
        Some(index) => (index + 1) % count,
        None => 0,
    };
    let target = state.focusables[next].clone();
    set_focus(state, Some(target), value_of, events);
}

fn close_selects(state: &mut UiState, keep: Option<&str>) {
    for (id, select) in state.selects.iter_mut() {
        if keep != Some(id.as_str()) {
            select.open = false;
        }
    }
}

fn set_focus(
    state: &mut UiState,
    target: Option<String>,
    value_of: &impl Fn(&str) -> Option<String>,
    events: &mut Vec<UiEvent>,
) {
    if state.focus == target {
        return;
    }
    if let Some(old) = state.focus.take() {
        if let Some(item) = state.inputs.get_mut(&old) {
            item.preedit.clear();
        }
        events.push(UiEvent::Focus { id: old, focused: false });
    }
    if let Some(id) = target {
        if matches!(state.layouts.get(&id), Some(Layout::Input { .. } | Layout::TextArea { .. })) {
            let len = value_of(&id).map(|value| value.len()).unwrap_or(0);
            let item = state
                .inputs
                .entry(id.clone())
                .or_insert_with(|| InputState { caret: len, anchor: len, ..InputState::default() });
            item.preedit.clear();
        }
        events.push(UiEvent::Focus { id: id.clone(), focused: true });
        state.focus = Some(id);
        state.reset_blink();
    }
}

async fn clip_write(ctx: &Context, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Err(err) = ctx.clipboard_write(text).await {
        ctx.log_error(&format!("写剪贴板失败：{err}"));
    }
}

fn boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn prev_boundary(value: &str, index: usize) -> usize {
    let index = boundary(value, index);
    if index == 0 {
        return 0;
    }
    let mut previous = index - 1;
    while previous > 0 && !value.is_char_boundary(previous) {
        previous -= 1;
    }
    previous
}

fn next_boundary(value: &str, index: usize) -> usize {
    let mut next = boundary(value, index) + 1;
    let len = value.len();
    if next >= len {
        return len;
    }
    while next < len && !value.is_char_boundary(next) {
        next += 1;
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_edges_stop_at_whitespace() {
        assert_eq!(word_edge("hello world", 11, true), 6);
        assert_eq!(word_edge("hello world", 0, false), 5);
        assert_eq!(word_edge("中文 词", 9, true), 0);
        assert_eq!(word_edge("中文 词", 10, true), 7);
        assert_eq!(word_edge("中文 词", 7, false), 10);
        // 词中位置先拼回词首 / 词尾
        assert_eq!(word_edge("one two", 5, true), 4);
        assert_eq!(word_edge("one two", 5, false), 7);
    }

    #[test]
    fn word_range_covers_the_token_under_the_caret() {
        assert_eq!(word_range("你好 世界", 0), (0, 6));
        assert_eq!(word_range("你好 世界", 3), (0, 6));
        assert_eq!(word_range("你好 世界", 6), (0, 6));
        assert_eq!(word_range("你好 世界", 7), (7, 13));
        // 空白段：向两侧取整段空白
        assert_eq!(word_range("a  b", 2), (1, 3));
    }

    #[test]
    fn boundaries_step_by_one_char() {
        assert_eq!(next_boundary("中文", 0), 3);
        assert_eq!(next_boundary("中文", 3), 6);
        assert_eq!(prev_boundary("中文", 6), 3);
        assert_eq!(prev_boundary("中文", 3), 0);
        assert_eq!(next_boundary("ab", 0), 1);
        assert_eq!(prev_boundary("ab", 1), 0);
    }

    #[test]
    fn insert_replaces_the_selection() {
        let mut state = UiState::default();
        state.layouts.insert("field".into(), Layout::Input { inner: Rect::default(), font: FontSpec { size: 12.0, weight: None, family: None }, value: "abcd".into() });
        let item = state.inputs.entry("field".into()).or_default();
        item.anchor = 1;
        item.caret = 3;
        let value = insert(&mut state, "field", "abcd", "中").unwrap();
        assert_eq!(value, "a中d");
        let item = state.inputs.get("field").unwrap();
        assert_eq!((item.caret, item.anchor), (4, 4), "光标落在插入内容之后");
    }

    #[test]
    fn newline_is_inserted_at_the_caret() {
        let mut state = UiState::default();
        let item = state.inputs.entry("body".into()).or_default();
        item.caret = 2;
        item.anchor = 2;
        let value = apply_edit(&mut state, "body", "abcd", Edit::Insert("\n".into())).unwrap();
        assert_eq!(value, "ab\ncd");
        assert_eq!(state.inputs.get("body").unwrap().caret, 3);
    }
}
