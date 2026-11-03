//! 窗口级插件浮层的根层：在所有面板之后绘制，并把指针输入按面本地坐标转发给归属插件。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Instant;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{self, Widget};
use iced::advanced::Shell;
use iced::event::Event;
use iced::{keyboard, mouse};
use iced::window;
use iced::{Element, Length, Point, Rectangle, Size};

use atelyx_core::plugin_runtime::{
    FrameInput, Modifiers as FrameModifiers, MouseButton, PopupCapture, PopupLayer, SurfaceFrame,
    SurfacePopup,
};

use crate::plugin_frame::{
    distance_sq, frame_modifiers, interaction_of, mouse_button, DOUBLE_CLICK_MS, DOUBLE_CLICK_SLOP,
};
use crate::plugin_paint::PaintCtx;
use crate::{App, Message};

/// 一个窗口级浮层的归属与几何（坐标仍是面本地，原点补成窗口坐标）。
struct Target {
    plugin_id: String,
    surface_id: String,
    origin: Point,
    popup: SurfacePopup,
}

impl Target {
    fn abs_rect(&self) -> Rectangle {
        Rectangle::new(
            Point::new(
                self.origin.x + self.popup.rect.x,
                self.origin.y + self.popup.rect.y,
            ),
            Size::new(self.popup.rect.w.max(0.0), self.popup.rect.h.max(0.0)),
        )
    }

    fn local(&self, absolute: Point) -> Point {
        local_point(self.origin, absolute)
    }

    fn hit(&self, local: Point) -> Option<String> {
        self.popup
            .hit_region_at(local.x, local.y)
            .map(|hit| hit.id.clone())
    }
}

/// 按下时定下归属：拖动与抬起都回给同一面，即使指针已移出浮层。
struct Press {
    plugin_id: String,
    surface_id: String,
    origin: Point,
    button: MouseButton,
    last: Point,
}

/// 窗口坐标 → 面本地坐标。
fn local_point(origin: Point, absolute: Point) -> Point {
    Point::new(absolute.x - origin.x, absolute.y - origin.y)
}

/// 命中窗口级浮层的按下归属。
#[derive(Debug, PartialEq)]
enum PressRoute {
    /// 命中浮层矩形：吞下事件并回发给该面。
    Inside { target: usize, local: Point },
    /// 未命中任何浮层，且存在整窗捕获浮层：吞下事件并回发窗口坐标的 Outside。
    Outside { target: usize, absolute: Point },
    /// 未命中任何浮层，也没有整窗捕获浮层：不吞，继续下发给面板。
    Pass,
}

/// 最靠上的命中浮层（`targets` 按绘制顺序升序，末位最靠上）。
fn topmost_index(targets: &[Target], point: Point) -> Option<usize> {
    targets
        .iter()
        .rposition(|target| target.abs_rect().contains(point))
}

/// 最靠上的整窗捕获浮层。
fn capture_all_index(targets: &[Target]) -> Option<usize> {
    targets
        .iter()
        .rposition(|target| target.popup.capture == PopupCapture::All)
}

/// 按下归属：浮层内命中优先；否则看最靠上的整窗捕获浮层，决定吞下还是放行。
fn press_route(targets: &[Target], point: Point) -> PressRoute {
    if let Some(target) = topmost_index(targets, point) {
        return PressRoute::Inside { target, local: local_point(targets[target].origin, point) };
    }
    match capture_all_index(targets) {
        Some(target) => PressRoute::Outside { target, absolute: point },
        None => PressRoute::Pass,
    }
}

/// 面帧 → 窗口级浮层归属；非窗口层或面原点未上报时跳过（不绘制、不接输入）。
fn window_target(
    plugin_id: &str,
    surface_id: &str,
    frame: &SurfaceFrame,
    origin: Option<Point>,
) -> Option<Target> {
    let popup = frame.popup.as_ref()?;
    if popup.layer != PopupLayer::Window {
        return None;
    }
    Some(Target {
        plugin_id: plugin_id.to_string(),
        surface_id: surface_id.to_string(),
        origin: origin?,
        popup: popup.clone(),
    })
}

#[derive(Default)]
struct OverlayState {
    /// 每个面的浮层绘制缓存（只保留当前在窗的浮层，卸载即淘汰）。
    paint: RefCell<HashMap<(String, String), crate::plugin_frame::PopupPaint>>,
    press: RefCell<Option<Press>>,
    hover: RefCell<Option<(String, String)>>,
    last_press: Cell<Option<(MouseButton, Instant, Point)>>,
    /// 最近一次按键携带的修饰键；滚轮事件不带修饰键，按它补全
    mods: Cell<FrameModifiers>,
}

/// 当前窗口里的窗口级浮层（无则返回 `None`，不额外加部件）。
pub fn overlay<'a>(app: &'a App, window: window::Id) -> Option<Element<'a, Message>> {
    let mut targets: Vec<Target> = app
        .plugin_frames
        .iter()
        .filter_map(|((plugin_id, surface_id), frame)| {
            let origin = app
                .plugin_surface_origins
                .get(&(window, plugin_id.clone(), surface_id.clone()))
                .copied();
            window_target(plugin_id, surface_id, frame, origin)
        })
        .collect();
    if targets.is_empty() {
        return None;
    }
    // 越大越靠上；同序按面 id 稳定排序，保证叠放与命中判定一致且可复现
    targets.sort_by(|a, b| {
        a.popup
            .order
            .cmp(&b.popup.order)
            .then(a.plugin_id.cmp(&b.plugin_id))
            .then(a.surface_id.cmp(&b.surface_id))
    });
    Some(Element::new(PluginOverlay { app, window, targets }))
}

struct PluginOverlay<'a> {
    app: &'a App,
    window: window::Id,
    /// 按绘制顺序升序（末位最靠上）。
    targets: Vec<Target>,
}

impl PluginOverlay<'_> {
    fn topmost_at(&self, point: Point) -> Option<&Target> {
        topmost_index(&self.targets, point).map(|index| &self.targets[index])
    }

    fn focused(&self, target: &Target) -> bool {
        self.app.plugin_frame_focus.as_ref().is_some_and(|focus| {
            focus.window == self.window
                && focus.plugin_id == target.plugin_id
                && focus.surface_id == target.surface_id
        })
    }

    fn focus(&self, target: &Target, shell: &mut Shell<'_, Message>) {
        self.app.plugins.notify_frame_input(
            &target.plugin_id,
            &target.surface_id,
            &FrameInput::Focus { focused: true },
        );
        shell.publish(Message::PluginFrameFocus {
            window: self.window,
            plugin_id: target.plugin_id.clone(),
            surface_id: target.surface_id.clone(),
            focused: true,
        });
        // 聚焦后须再绘一帧：输入法在绘制期启用
        shell.request_redraw();
    }

    fn send(&self, plugin_id: &str, surface_id: &str, input: FrameInput) {
        self.app
            .plugins
            .notify_frame_input(plugin_id, surface_id, &input);
    }

    fn target_by_key(&self, key: &(String, String)) -> Option<&Target> {
        self.targets
            .iter()
            .find(|target| &target.plugin_id == &key.0 && &target.surface_id == &key.1)
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for PluginOverlay<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<OverlayState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(OverlayState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut widget::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fill, Length::Fill)
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<OverlayState>();
        let mut paint = state.paint.borrow_mut();
        // 面已卸载的浮层缓存不再保留，避免按面累积
        let live: std::collections::HashSet<_> = self
            .targets
            .iter()
            .map(|target| (target.plugin_id.clone(), target.surface_id.clone()))
            .collect();
        paint.retain(|key, _| live.contains(key));
        for target in &self.targets {
            let cache = paint
                .entry((target.plugin_id.clone(), target.surface_id.clone()))
                .or_default();
            let ctx = PaintCtx { app: self.app, plugin_id: &target.plugin_id };
            cache.draw(
                &target.popup,
                target.origin,
                viewport.size(),
                *viewport,
                renderer,
                &ctx,
            );
        }
        let _ = layout;
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<OverlayState>();
        let _ = (layout, clipboard);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                if shell.is_event_captured() {
                    return;
                }
                let Some(mouse_button) = mouse_button(*button) else {
                    return;
                };
                if state.press.borrow().is_some() {
                    return;
                }
                let Some(point) = cursor.position() else {
                    return;
                };
                match press_route(&self.targets, point) {
                    PressRoute::Inside { target, local } => {
                        let target = &self.targets[target];
                        if !self.focused(target) {
                            self.focus(target, shell);
                        }
                        let now = Instant::now();
                        let last = state.last_press.get();
                        let double_click = last.is_some_and(|(last_button, at, last_point)| {
                            last_button == mouse_button
                                && now.duration_since(at).as_millis() < u128::from(DOUBLE_CLICK_MS)
                                && distance_sq(point, last_point) < DOUBLE_CLICK_SLOP * DOUBLE_CLICK_SLOP
                        });
                        state
                            .last_press
                            .set(if double_click { None } else { Some((mouse_button, now, point)) });
                        *state.press.borrow_mut() = Some(Press {
                            plugin_id: target.plugin_id.clone(),
                            surface_id: target.surface_id.clone(),
                            origin: target.origin,
                            button: mouse_button,
                            last: local,
                        });
                        let hit = target.hit(local);
                        let input = if double_click {
                            FrameInput::DoubleClick {
                                x: local.x,
                                y: local.y,
                                button: mouse_button,
                                hit,
                            }
                        } else {
                            FrameInput::PointerDown { x: local.x, y: local.y, button: mouse_button, hit }
                        };
                        self.send(&target.plugin_id, &target.surface_id, input);
                        shell.capture_event();
                    }
                    PressRoute::Outside { target, absolute } => {
                        // 整窗捕获浮层外按下：位置用窗口坐标回传，事件不再落到下层面板
                        let target = &self.targets[target];
                        self.send(
                            &target.plugin_id,
                            &target.surface_id,
                            FrameInput::Outside { x: absolute.x, y: absolute.y },
                        );
                        shell.capture_event();
                    }
                    PressRoute::Pass => {}
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let Some(mouse_button) = mouse_button(*button) else {
                    return;
                };
                let pressed = state.press.borrow().as_ref().map(|press| press.button);
                if pressed != Some(mouse_button) {
                    return;
                }
                let Some(press) = state.press.borrow_mut().take() else {
                    return;
                };
                let local = cursor
                    .position()
                    .map(|point| Point::new(point.x - press.origin.x, point.y - press.origin.y))
                    .unwrap_or(press.last);
                let key = (press.plugin_id.clone(), press.surface_id.clone());
                let hit = self.target_by_key(&key).and_then(|target| target.hit(local));
                self.send(
                    &press.plugin_id,
                    &press.surface_id,
                    FrameInput::PointerUp { x: local.x, y: local.y, button: mouse_button, hit },
                );
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(point) = cursor.position() else {
                    return;
                };
                if let Some(press) = state.press.borrow_mut().as_mut() {
                    let local = Point::new(point.x - press.origin.x, point.y - press.origin.y);
                    press.last = local;
                    let key = (press.plugin_id.clone(), press.surface_id.clone());
                    let hit = self.target_by_key(&key).and_then(|target| target.hit(local));
                    self.send(
                        &press.plugin_id,
                        &press.surface_id,
                        FrameInput::PointerMove { x: local.x, y: local.y, hit },
                    );
                    shell.capture_event();
                    return;
                }
                let hover = self.topmost_at(point);
                let next_key = hover.map(|target| (target.plugin_id.clone(), target.surface_id.clone()));
                let previous = state.hover.replace(next_key.clone());
                if let Some((plugin_id, surface_id)) = previous {
                    if next_key.as_ref() != Some(&(plugin_id.clone(), surface_id.clone())) {
                        self.send(&plugin_id, &surface_id, FrameInput::PointerLeave);
                    }
                }
                if let Some(target) = hover {
                    let local = target.local(point);
                    let hit = target.hit(local);
                    self.send(
                        &target.plugin_id,
                        &target.surface_id,
                        FrameInput::PointerMove { x: local.x, y: local.y, hit },
                    );
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if let Some((plugin_id, surface_id)) = state.hover.take() {
                    self.send(&plugin_id, &surface_id, FrameInput::PointerLeave);
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(point) = cursor.position() else {
                    return;
                };
                let Some(target) = self.topmost_at(point) else {
                    return;
                };
                let local = target.local(point);
                let (dx, dy) = match delta {
                    mouse::ScrollDelta::Lines { x, y } => (
                        -x * crate::canvas_view::WHEEL_LINE_PX,
                        -y * crate::canvas_view::WHEEL_LINE_PX,
                    ),
                    mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
                };
                let hit = target.hit(local);
                self.send(
                    &target.plugin_id,
                    &target.surface_id,
                    FrameInput::Wheel {
                        x: local.x,
                        y: local.y,
                        delta_x: dx,
                        delta_y: dy,
                        modifiers: state.mods.get(),
                        hit,
                    },
                );
                shell.capture_event();
            }
            Event::Keyboard(keyboard::Event::KeyPressed { modifiers, .. })
            | Event::Keyboard(keyboard::Event::KeyReleased { modifiers, .. })
            | Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.mods.set(frame_modifiers(*modifiers));
            }
            Event::Window(window::Event::Unfocused) => {
                // 失焦后收不到抬起：补发 PointerUp，浮层归属插件不会卡在按下态
                if let Some(press) = state.press.borrow_mut().take() {
                    let key = (press.plugin_id.clone(), press.surface_id.clone());
                    let hit = self.target_by_key(&key).and_then(|target| target.hit(press.last));
                    self.send(
                        &press.plugin_id,
                        &press.surface_id,
                        FrameInput::PointerUp {
                            x: press.last.x,
                            y: press.last.y,
                            button: press.button,
                            hit,
                        },
                    );
                }
                if let Some((plugin_id, surface_id)) = state.hover.take() {
                    self.send(&plugin_id, &surface_id, FrameInput::PointerLeave);
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &widget::Tree,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let Some(point) = cursor.position() else {
            return mouse::Interaction::None;
        };
        let Some(target) = self.topmost_at(point) else {
            return mouse::Interaction::None;
        };
        let local = target.local(point);
        match target.popup.hit_region_at(local.x, local.y) {
            Some(hit) => interaction_of(hit.cursor.unwrap_or_default()),
            // 浮层空白处也不能露出下层面板的输入光标
            None => mouse::Interaction::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        capture_all_index, local_point, press_route, topmost_index, window_target, PressRoute,
        Target,
    };
    use atelyx_core::plugin_runtime::{
        FrameRect, PopupCapture, PopupLayer, SurfaceFrame, SurfacePopup,
    };
    use iced::Point;

    fn popup(rect: (f32, f32, f32, f32), capture: PopupCapture, order: u16) -> SurfacePopup {
        SurfacePopup {
            rect: FrameRect { x: rect.0, y: rect.1, w: rect.2, h: rect.3 },
            capture,
            layer: PopupLayer::Window,
            order,
            commands: Vec::new(),
            hits: Vec::new(),
            ime: None,
        }
    }

    fn target(origin: Point, popup: SurfacePopup) -> Target {
        Target {
            plugin_id: "com.atelyx.test".into(),
            surface_id: "surface".into(),
            origin,
            popup,
        }
    }

    fn frame_with(popup: SurfacePopup) -> SurfaceFrame {
        SurfaceFrame { popup: Some(popup), ..SurfaceFrame::default() }
    }

    #[test]
    fn local_point_subtracts_face_origin() {
        assert_eq!(
            local_point(Point::new(100.0, 50.0), Point::new(130.0, 70.0)),
            Point::new(30.0, 20.0)
        );
    }

    #[test]
    fn all_capture_swallows_outside_press_while_inside_capture_passes_it() {
        let inside =
            vec![target(Point::ORIGIN, popup((0.0, 0.0, 100.0, 100.0), PopupCapture::Inside, 0))];
        // 浮层内按下：两种 capture 都由浮层处理
        assert_eq!(
            press_route(&inside, Point::new(15.0, 25.0)),
            PressRoute::Inside { target: 0, local: Point::new(15.0, 25.0) }
        );
        // Inside 的浮层外按下不吞，继续交给下层面板
        assert_eq!(press_route(&inside, Point::new(200.0, 25.0)), PressRoute::Pass);

        let all = vec![target(Point::ORIGIN, popup((0.0, 0.0, 100.0, 100.0), PopupCapture::All, 0))];
        // All 的浮层外按下吞掉，并以窗口坐标回发 Outside
        assert_eq!(
            press_route(&all, Point::new(200.0, 25.0)),
            PressRoute::Outside { target: 0, absolute: Point::new(200.0, 25.0) }
        );
    }

    #[test]
    fn only_topmost_window_popup_takes_the_press() {
        let targets = vec![
            target(Point::ORIGIN, popup((0.0, 0.0, 100.0, 100.0), PopupCapture::Inside, 1)),
            target(Point::ORIGIN, popup((50.0, 50.0, 100.0, 100.0), PopupCapture::Inside, 2)),
        ];
        // 重叠区只归最高层
        assert_eq!(topmost_index(&targets, Point::new(60.0, 60.0)), Some(1));
        assert_eq!(
            press_route(&targets, Point::new(60.0, 60.0)),
            PressRoute::Inside { target: 1, local: Point::new(60.0, 60.0) }
        );
        // 只落在低层矩形内时归低层
        assert_eq!(topmost_index(&targets, Point::new(10.0, 10.0)), Some(0));
        // 都不在
        assert_eq!(topmost_index(&targets, Point::new(200.0, 200.0)), None);
    }

    #[test]
    fn topmost_capture_all_popup_receives_outside_press() {
        let targets = vec![
            target(Point::ORIGIN, popup((0.0, 0.0, 50.0, 50.0), PopupCapture::All, 1)),
            target(Point::ORIGIN, popup((100.0, 0.0, 50.0, 50.0), PopupCapture::All, 2)),
        ];
        assert_eq!(capture_all_index(&targets), Some(1));
        assert_eq!(
            press_route(&targets, Point::new(500.0, 10.0)),
            PressRoute::Outside { target: 1, absolute: Point::new(500.0, 10.0) }
        );
    }

    #[test]
    fn missing_surface_origin_skips_window_popup_without_panicking() {
        let frame = frame_with(popup((0.0, 0.0, 100.0, 100.0), PopupCapture::All, 0));
        // 原点未上报：跳过该浮层，不绘制也不接输入，且不 panic
        assert!(window_target("com.atelyx.test", "surface", &frame, None).is_none());
        assert!(
            window_target("com.atelyx.test", "surface", &frame, Some(Point::new(3.0, 4.0)))
                .is_some()
        );

        // 面板内层与非浮层帧都不进窗口级浮层
        let panel = SurfacePopup { layer: PopupLayer::Panel, ..popup((0.0, 0.0, 100.0, 100.0), PopupCapture::All, 0) };
        assert!(window_target("com.atelyx.test", "surface", &frame_with(panel), Some(Point::ORIGIN))
            .is_none());
        assert!(
            window_target("com.atelyx.test", "surface", &SurfaceFrame::default(), Some(Point::ORIGIN))
                .is_none()
        );
    }
}
