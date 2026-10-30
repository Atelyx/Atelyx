//! 原始输入 → 语义事件：指针按命中 id 与上次布局解释成点击 / 聚焦，键盘与输入法落到聚焦控件上编辑。

use atelyx_plugin_protocol::frame::FrameInput;
use atelyx_plugin_protocol::frame::{Key, Modifiers, MouseButton, NamedKey};
use atelyx_plugin_protocol::method::MAX_MEASURE_ITEMS;

use super::geom::Rect;
use super::model::{Drag, FontSpec, InputState, Layout, Press, UiState};
use super::text;
use crate::frame_ui::UiEvent;
use crate::Context;

/// 自绘子区起拖的最小位移（逻辑像素）：小于它仍按点击 / 双击解释。
const DRAG_THRESHOLD: f32 = 4.0;

/// 一条输入对应的语义事件；改完状态后由插件调用 `render` 提交新一帧。
pub(crate) async fn handle(
    state: &mut UiState,
    ctx: &Context,
    input: &FrameInput,
    value_of: impl Fn(&str) -> Option<String>,
) -> Vec<UiEvent> {
    let mut events = Vec::new();
    match input {
        FrameInput::PointerDown { x, y, button, hit, .. } => {
            if *button == MouseButton::Right {
                // 右键不改焦点与按下态：只把落点控件报给插件（行内右键菜单 / 快捷动作）
                if let Some(id) = hit.as_deref() {
                    events.push(UiEvent::RightClick { id: id.to_string() });
                }
            } else {
                pointer_down(state, ctx, *x, *y, *button, hit.as_deref(), &value_of, &mut events).await;
            }
        }
        FrameInput::PointerUp { x, y, hit, .. } => pointer_up(state, *x, *y, hit.as_deref(), &mut events),
        FrameInput::DoubleClick { x, y, button, hit, .. } => {
            if *button == MouseButton::Right {
                // 右键双击按一次右键处理：不改焦点与按下态
                if let Some(id) = hit.as_deref() {
                    events.push(UiEvent::RightClick { id: id.to_string() });
                }
            } else {
                pointer_down(state, ctx, *x, *y, *button, hit.as_deref(), &value_of, &mut events).await;
                if let Some(id) = hit
                    .as_deref()
                    .filter(|id| matches!(state.layouts.get(*id), Some(Layout::Input { .. } | Layout::TextArea { .. })))
                {
                    if let Some(value) = value_of(id) {
                        let item = state.inputs.entry(id.to_string()).or_default();
                        item.caret = value.len();
                        item.anchor = 0;
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
            pointer_move(state, ctx, *x, *y, hit.as_deref(), &value_of, &mut events).await;
        }
        FrameInput::PointerLeave => state.hovered = None,
        FrameInput::Wheel { x, y, delta_y, .. } => {
            if let Some(event) = wheel(state, *x, *y, *delta_y) {
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
    events
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
    // 新的一次按下换掉上一轮拖拽候选：拖拽只在同一次按下内成立
    state.drag = None;
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
        Layout::TextArea { inner, font, value: shown, .. } => {
            // 可视首行取决于落点前的光标所在行：先按旧光标换算，再改光标与焦点
            let current = value_of(id).unwrap_or(shown);
            let was_focused = state.focus.as_deref() == Some(id);
            let old = state.inputs.get(id).map(|item| item.caret).unwrap_or(0);
            let caret = area_caret_at(state, ctx, inner, font, &current, x, y, old, was_focused).await;
            state.inputs.insert(id.to_string(), InputState { caret, anchor: caret, preedit: String::new() });
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
        Layout::Button | Layout::Option { .. } => {
            state.press = Some(Press { id: id.to_string(), from_input: false });
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
            state.drag_bar = Some(scroll.clone());
            let offset = bar_offset(track, thumb, max_offset, y);
            state.scroll.insert(scroll.clone(), offset);
            events.push(UiEvent::Scroll { id: scroll, offset, max_offset });
        }
    }
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
        Some(Layout::Button) | Some(Layout::Checkbox) | Some(Layout::Custom) => Outcome::Click,
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
    if let Some(scroll) = state.drag_bar.clone() {
        let bar = super::widgets::scrollbar_id(&scroll);
        if let Some(Layout::ScrollBar { track, thumb, max_offset, .. }) = state.layouts.get(&bar).cloned() {
            let offset = bar_offset(track, thumb, max_offset, y);
            if (state.scroll.get(&scroll).copied().unwrap_or(0.0) - offset).abs() >= 0.01 {
                state.scroll.insert(scroll.clone(), offset);
                events.push(UiEvent::Scroll { id: scroll, offset, max_offset });
            }
            return;
        }
        state.drag_bar = None;
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
        Layout::TextArea { inner, font, value: shown, .. } => {
            let current = value_of(&press.id).unwrap_or(shown);
            let old = state.inputs.get(&press.id).map(|item| item.caret).unwrap_or(0);
            area_caret_at(state, ctx, inner, font, &current, x, y, old, true).await
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

/// 滚轮改滚动偏移：增量是像素（宿主已把「行」换算成像素），按光标所在的最内层滚动容器走。
fn wheel(state: &mut UiState, x: f32, y: f32, delta_y: f32) -> Option<UiEvent> {
    // 嵌套滚动区取面积最小者（最内层）：否则滚动会作用到外层容器上
    let (id, content, viewport) = state
        .layouts
        .iter()
        .filter_map(|(id, layout)| match layout {
            Layout::Scroll { viewport, content } if viewport.contains(x, y) => {
                Some((id.clone(), *content, *viewport))
            }
            _ => None,
        })
        .min_by(|a, b| (a.2.w * a.2.h).total_cmp(&(b.2.w * b.2.h)))?;
    let max_offset = (content - viewport.h).max(0.0);
    let current = state.scroll.get(&id).copied().unwrap_or(0.0);
    let next = (current + delta_y).clamp(0.0, max_offset);
    if (next - current).abs() < 0.01 {
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
    // 拖拽中的 Esc 先取消拖拽（未起拖的候选只清掉，不影响后续按键语义）
    if matches!(key, Key::Named { name: NamedKey::Escape }) {
        if let Some(drag) = state.drag.take().filter(|drag| drag.started) {
            state.press = None;
            events.push(UiEvent::DragCancel { id: drag.id });
            return;
        }
    }
    let Some(id) = state.focus.clone() else {
        return;
    };
    let Some(layout) = state.layouts.get(&id).cloned() else {
        return;
    };
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
        Layout::Input { .. } => edit_key(state, ctx, &id, key, modifiers, text, value_of, events, None).await,
        Layout::TextArea { inner, font, .. } => {
            edit_key(state, ctx, &id, key, modifiers, text, value_of, events, Some((inner, font))).await
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
    area: Option<(Rect, FontSpec)>,
) {
    let Some(current) = current_value(state, id, value_of) else {
        return;
    };
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
                        item.caret = from;
                        item.anchor = from;
                        let value = format!("{}{}", &current[..from], &current[to..]);
                        events.push(UiEvent::Text { id: id.to_string(), value });
                    }
                }
                "v" => match ctx.clipboard_read().await {
                    Ok(text) => {
                        if let Some(value) = insert(state, id, &current, &text) {
                            events.push(UiEvent::Text { id: id.to_string(), value });
                        }
                    }
                    Err(err) => eprintln!("读取剪贴板失败：{err}"),
                },
                _ => {}
            }
        }
        return;
    }
    if let Some((inner, font)) = area {
        // 多行的上下键与行首尾按可视行走：按整篇首尾跳会跨过一整段
        if let Key::Named { name: NamedKey::ArrowUp | NamedKey::ArrowDown } = key {
            let up = matches!(key, Key::Named { name: NamedKey::ArrowUp });
            move_vertical(state, ctx, id, &current, inner, font, up, modifiers.shift).await;
            return;
        }
        if let Key::Named { name: NamedKey::Home | NamedKey::End } = key {
            let end = matches!(key, Key::Named { name: NamedKey::End });
            move_line_edge(state, ctx, id, &current, inner, font, end, modifiers.shift).await;
            return;
        }
    }
    let edit = match key {
        Key::Character { .. } => text.map(|text| Edit::Insert(text.to_string())),
        Key::Named { name } => match name {
            NamedKey::Backspace => Some(Edit::Backspace),
            NamedKey::Delete => Some(Edit::Delete),
            NamedKey::Enter => area.map(|_| Edit::Insert("\n".to_string())),
            NamedKey::ArrowLeft => Some(Edit::Left { extend: modifiers.shift }),
            NamedKey::ArrowRight => Some(Edit::Right { extend: modifiers.shift }),
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
        return;
    }
    let Some(current) = current_value(state, &id, value_of) else {
        return;
    };
    if let Some(item) = state.inputs.get_mut(&id) {
        item.preedit.clear();
    }
    if text.is_empty() {
        return;
    }
    if let Some(value) = insert(state, &id, &current, text) {
        events.push(UiEvent::Text { id, value });
    }
}

enum Edit {
    Insert(String),
    Backspace,
    Delete,
    Left { extend: bool },
    Right { extend: bool },
    Home { extend: bool },
    End { extend: bool },
}

fn apply_edit(state: &mut UiState, id: &str, value: &str, edit: Edit) -> Option<String> {
    let item = state.inputs.entry(id.to_string()).or_default();
    let caret = boundary(value, item.caret.min(value.len()));
    let anchor = boundary(value, item.anchor.min(value.len()));
    match edit {
        Edit::Insert(text) => insert(state, id, value, &text),
        Edit::Backspace => {
            let (from, to) = if caret != anchor { (caret.min(anchor), caret.max(anchor)) } else {
                (prev_boundary(value, caret), caret)
            };
            if from == to {
                return None;
            }
            let item = state.inputs.entry(id.to_string()).or_default();
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
            item.caret = from;
            item.anchor = from;
            Some(format!("{}{}", &value[..from], &value[to..]))
        }
        Edit::Left { extend } => {
            let item = state.inputs.entry(id.to_string()).or_default();
            if !extend && caret != anchor {
                item.caret = caret.min(anchor);
                item.anchor = item.caret;
            } else {
                item.caret = prev_boundary(value, caret);
                if !extend {
                    item.anchor = item.caret;
                }
            }
            None
        }
        Edit::Right { extend } => {
            let item = state.inputs.entry(id.to_string()).or_default();
            if !extend && caret != anchor {
                item.caret = caret.max(anchor);
                item.anchor = item.caret;
            } else {
                item.caret = next_boundary(value, caret);
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
                eprintln!("量算失败，按估算字宽落光标：{err}");
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

/// 多行输入的落点换算：`focused` 决定纵向平移量（未聚焦时画面停在前几行）。
#[allow(clippy::too_many_arguments)]
async fn area_caret_at(
    state: &mut UiState,
    ctx: &Context,
    inner: Rect,
    font: FontSpec,
    value: &str,
    x: f32,
    y: f32,
    old_caret: usize,
    focused: bool,
) -> usize {
    refill(state, ctx, value, font, inner.w).await;
    let lines = text::lines(&mut state.metrics, value, font, inner.w);
    let line_height = state.metrics.measure("", font, None).height.max(1.0);
    let top = if focused {
        text::top_line(text::line_of(&lines, text::snap(value, old_caret)), line_height, inner.h)
    } else {
        0
    };
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
                .or_insert_with(|| InputState { caret: len, anchor: len, preedit: String::new() });
            item.preedit.clear();
        }
        events.push(UiEvent::Focus { id: id.clone(), focused: true });
        state.focus = Some(id);
    }
}

async fn clip_write(ctx: &Context, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Err(err) = ctx.clipboard_write(text).await {
        eprintln!("写剪贴板失败：{err}");
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
