//! 统一弹层壳：锚点定位 + 视口钳制/翻转 + 外点关闭的窗口内覆盖层。
//! 覆盖元素由调用方作为追加子叠在窗口内容 Stack 上，Esc 由应用层键盘订阅路由。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{container, mouse_area, opaque, space, stack};
use iced::{Color, Element, Length, Padding, Point, Rectangle, Size, border, widget::container::Style};

use super::palette as pal;
use super::{UiHost, UiMsg};

/// 弹层规格：锚点（窗口局部坐标）+ 定宽 + 估算高 + 向上翻转的触发器顶边。
#[derive(Clone, Copy, Debug)]
pub struct PopupSpec {
    pub anchor: Point,
    pub width: f32,
    pub height: f32,
    /// 触发器顶边 y（下方放不下且上方放得下时，底边贴此值翻转）。
    pub flip_y: Option<f32>,
    /// 宽度下限（弹层不窄于触发按钮；0 = 不限）。
    pub min_width: f32,
}

/// 落位边界：可用区域与四边留白（各边独立；可把弹层收在一段面板区内）。
#[derive(Clone, Copy, Debug)]
pub struct PopupBounds {
    /// 可用区域；留白自该区域四边向内收。
    pub area: Rectangle,
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// 竖直落位偏好。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VFlip {
    /// 下方放不下且上方放得下才翻到上方（弹层默认）。
    BelowFirst,
    /// 上方放得下就放上方，否则放下方（格式气泡）。
    AboveFirst,
    /// 恒在上方：贴触发器上缘展开，上方放不下就钳到可用区顶（不换边）。
    /// 给"换子面板时高度变化"的菜单用：换边跳变比贴顶更碍事。
    Above,
}

/// 解析后的弹层矩形（窗口局部坐标，已钳制/翻转）。
pub struct PopupGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// 钳制 + 翻转（视口边距 6px；下方放不下且上方放得下才翻转）。
pub fn resolve(spec: &PopupSpec, viewport: Size) -> PopupGeom {
    resolve_preferring(spec, viewport, VFlip::BelowFirst)
}

/// 视口内落位，带竖直翻转偏好（底缘触发的菜单用 `VFlip::AboveFirst` 贴钮上方展开）。
pub fn resolve_preferring(spec: &PopupSpec, viewport: Size, flip: VFlip) -> PopupGeom {
    // 宽度同样受视口约束（窄窗口下长标签弹层不能横向溢出）
    let max_w = (viewport.width - pal::VIEWPORT_GAP * 2.0).max(0.0);
    let w = spec.width.max(spec.min_width).min(max_w);
    place(
        spec,
        w,
        PopupBounds {
            area: Rectangle::new(Point::ORIGIN, viewport),
            left: pal::VIEWPORT_GAP,
            top: pal::VIEWPORT_GAP,
            right: pal::VIEWPORT_GAP,
            bottom: pal::VIEWPORT_GAP,
        },
        flip,
    )
}

/// 自定义边界内的落位：与 [`resolve`] 共用翻转/钳制算式；宽度按规格取，不再按视口收窄。
pub fn resolve_in(spec: &PopupSpec, bounds: PopupBounds, flip: VFlip) -> PopupGeom {
    place(spec, spec.width.max(spec.min_width), bounds, flip)
}

/// 落位算式：位置钳在「可用区域 - 四边留白」内，翻转按偏好决定取上方还是下方候选。
fn place(spec: &PopupSpec, w: f32, bounds: PopupBounds, flip: VFlip) -> PopupGeom {
    let h = spec.height;
    let min_x = bounds.area.x + bounds.left;
    let max_x = (bounds.area.x + bounds.area.width - w - bounds.right).max(min_x);
    let x = spec.anchor.x.clamp(min_x, max_x);
    let min_y = bounds.area.y + bounds.top;
    let max_y = (bounds.area.y + bounds.area.height - h - bounds.bottom).max(min_y);
    let mut y = spec.anchor.y;
    match flip {
        // 硬边：不看空间，贴触发器上缘（放不下由下方钳制收到可用区顶）
        VFlip::Above => {
            if let Some(flip_y) = spec.flip_y {
                y = flip_y - h;
            }
        }
        VFlip::BelowFirst | VFlip::AboveFirst => {
            if let Some(flip_y) = spec.flip_y {
                let above = flip_y - h;
                let above_fits = above >= min_y;
                let below_fits =
                    spec.anchor.y + h <= bounds.area.y + bounds.area.height - bounds.bottom;
                let use_above = match flip {
                    VFlip::BelowFirst => !below_fits && above_fits,
                    _ => above_fits,
                };
                if use_above {
                    y = above;
                }
            }
        }
    }
    PopupGeom { x, y: y.clamp(min_y, max_y), w }
}

/// 阴影环外扩量（左右/顶部/底部；与 positioned 偏移配对使用，调用方算可用高度时也按此扣）。
pub const SHADOW_PAD_LEFT: f32 = 4.0;
pub const SHADOW_PAD_TOP: f32 = 2.0;
pub const SHADOW_PAD_BOTTOM: f32 = 14.0;

/// 弹层覆盖元素（全窗捕获层 + 定位弹层卡）：由调用方作为覆盖子叠在窗口内容之上
/// （内容恒为 Stack 第 0 子，覆盖显隐不重建内容子树——滚动位置与输入焦点得以保留）。
///
/// 阴影用「外扩容器露出的半透明黑底环」近似（iced 无 box-shadow）；
/// 定位偏移 = 弹层坐标回退外扩量，两处必须同步（见常量）。
pub fn overlay<'a, M: Clone + 'a>(
    geom: PopupGeom,
    content: Element<'a, M>,
    on_dismiss: M,
) -> Element<'a, M> {
    let capture = mouse_area(space().width(Length::Fill).height(Length::Fill)).on_press(on_dismiss);
    // 卡片矩形内的按下由 opaque 截住：卡片非控件区域（内边距 / 条目间空白）
    // 不穿透到捕获层，外点判定只按卡片矩形（阴影环仍算外点）。
    let card = opaque(
        container(content)
            .width(Length::Fixed(geom.w))
            .style(move |_t| Style {
                background: Some(pal::BG_OVERLAY.into()),
                border: border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
                ..Style::default()
            }),
    );
    let shadow_wrap = container(card)
        .padding(Padding {
            left: SHADOW_PAD_LEFT,
            top: SHADOW_PAD_TOP,
            right: SHADOW_PAD_LEFT,
            bottom: SHADOW_PAD_BOTTOM,
        })
        .style(move |_t| Style {
            background: Some(Color::BLACK.scale_alpha(0.35).into()),
            border: border::rounded(pal::RADIUS_MD + SHADOW_PAD_LEFT),
            ..Style::default()
        });
    let positioned = container(shadow_wrap)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Left)
        .align_y(iced::alignment::Vertical::Top)
        .padding(Padding::ZERO.left(geom.x - SHADOW_PAD_LEFT).top(geom.y - SHADOW_PAD_TOP));
    stack![capture, positioned]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 非模态浮动卡：只做定位与阴影，不铺捕获层——外点是否关闭由调用方的状态机决定
/// （属性候选这类「外点即提交、且要照常落到被点控件」的浮层不能用捕获层吞按压）。
pub fn floating<'a, M: Clone + 'a>(geom: PopupGeom, content: Element<'a, M>) -> Element<'a, M> {
    let card = opaque(
        container(content)
            .width(Length::Fixed(geom.w))
            .style(move |_t| Style {
                background: Some(pal::BG_OVERLAY.into()),
                border: border::rounded(pal::RADIUS_SM).color(pal::BORDER).width(1.0),
                ..Style::default()
            }),
    );
    let shadow_wrap = container(card)
        .padding(Padding {
            left: SHADOW_PAD_LEFT,
            top: SHADOW_PAD_TOP,
            right: SHADOW_PAD_LEFT,
            bottom: SHADOW_PAD_BOTTOM,
        })
        .style(move |_t| Style {
            background: Some(Color::BLACK.scale_alpha(0.35).into()),
            border: border::rounded(pal::RADIUS_SM + SHADOW_PAD_LEFT),
            ..Style::default()
        });
    container(shadow_wrap)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Left)
        .align_y(iced::alignment::Vertical::Top)
        .padding(Padding::ZERO.left(geom.x - SHADOW_PAD_LEFT).top(geom.y - SHADOW_PAD_TOP))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(anchor: Point, height: f32, flip_y: f32) -> PopupSpec {
        PopupSpec {
            anchor,
            width: 100.0,
            height,
            flip_y: Some(flip_y),
            min_width: 0.0,
        }
    }

    /// 三种竖直偏好各自落位：默认放下方；AboveFirst 上方放得下才上翻；
    /// Above 恒贴触发器上缘，上方放不下就钳到可用区顶、不换边。
    #[test]
    fn vertical_preferences_place_popup_on_expected_side() {
        let viewport = Size::new(600.0, 400.0);
        let mid = spec(Point::new(100.0, 200.0), 80.0, 188.0);
        assert_eq!(resolve(&mid, viewport).y, 200.0, "默认放下方");
        assert_eq!(
            resolve_preferring(&mid, viewport, VFlip::AboveFirst).y,
            108.0,
            "上方放得下就贴触发器上缘"
        );
        assert_eq!(
            resolve_preferring(&mid, viewport, VFlip::Above).y,
            108.0,
            "硬边上方"
        );

        // 触发器靠近顶部：上方放不下
        let top = spec(Point::new(100.0, 60.0), 120.0, 48.0);
        assert_eq!(
            resolve_preferring(&top, viewport, VFlip::Above).y,
            pal::VIEWPORT_GAP,
            "硬边不换边，钳到可用区顶"
        );
        assert_eq!(
            resolve_preferring(&top, viewport, VFlip::AboveFirst).y,
            60.0,
            "AboveFirst 上方放不下就放下方"
        );
    }
}

/// 居中模态覆盖层：全屏半透明黑遮罩（点击关闭）+ 阴影环 + 水平垂直居中卡。
/// 卡片内容与尺寸由调用方构建（历史记录 / 许可信息等阻断式面板共用此壳）。
pub fn modal_overlay<'a, M: UiHost>(window: iced::window::Id, card: Element<'a, M>) -> Element<'a, M> {
    modal_overlay_with(M::ui(UiMsg::PopupClose { window }), card)
}

/// 居中模态覆盖层（遮罩点击的处理由调用方给定：不经全局弹层状态的模态用这条）。
pub fn modal_overlay_with<'a, M: Clone + 'a>(close: M, card: Element<'a, M>) -> Element<'a, M> {
    let capture = mouse_area(
        container(space().width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| Style {
                background: Some(pal::SCRIM.into()),
                ..Style::default()
            }),
    )
    .on_press(close);
    // 同 overlay：卡片矩形内按下不穿透遮罩（阴影环仍算外点）。
    let shadow_wrap = container(opaque(card))
        .padding(Padding {
            left: SHADOW_PAD_LEFT,
            top: SHADOW_PAD_TOP,
            right: SHADOW_PAD_LEFT,
            bottom: SHADOW_PAD_BOTTOM,
        })
        .style(|_t| Style {
            background: Some(Color::BLACK.scale_alpha(0.35).into()),
            border: border::rounded(pal::RADIUS_MD + SHADOW_PAD_LEFT),
            ..Style::default()
        });
    let centered = container(shadow_wrap)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center);
    stack![capture, centered]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
