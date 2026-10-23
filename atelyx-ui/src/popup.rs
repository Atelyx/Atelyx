//! 统一弹层壳：锚点定位 + 视口钳制/翻转 + 外点关闭的窗口内覆盖层。
//! 覆盖元素由调用方作为追加子叠在窗口内容 Stack 上，Esc 由应用层键盘订阅路由。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{container, mouse_area, space, stack};
use iced::{Color, Element, Length, Padding, Point, Size, border, widget::container::Style};

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

/// 解析后的弹层矩形（窗口局部坐标，已钳制/翻转）。
pub struct PopupGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// 钳制 + 翻转（视口边距 6px；下方放不下且上方放得下才翻转）。
pub fn resolve(spec: &PopupSpec, viewport: Size) -> PopupGeom {
    // 宽度同样受视口约束（窄窗口下长标签弹层不能横向溢出）
    let max_w = (viewport.width - pal::VIEWPORT_GAP * 2.0).max(0.0);
    let w = spec.width.max(spec.min_width).min(max_w);
    let h = spec.height;
    let max_x = (viewport.width - w - pal::VIEWPORT_GAP).max(pal::VIEWPORT_GAP);
    let x = spec.anchor.x.clamp(pal::VIEWPORT_GAP, max_x);
    let mut y = spec.anchor.y;
    if let Some(flip) = spec.flip_y {
        let below_fits = spec.anchor.y + h <= viewport.height - pal::VIEWPORT_GAP;
        let above_fits = flip - h >= pal::VIEWPORT_GAP;
        if !below_fits && above_fits {
            y = flip - h;
        }
    }
    let max_y = (viewport.height - h - pal::VIEWPORT_GAP).max(pal::VIEWPORT_GAP);
    let y = y.clamp(pal::VIEWPORT_GAP, max_y);
    PopupGeom { x, y, w }
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
    let popup = container(content)
        .width(Length::Fixed(geom.w))
        .style(move |_t| Style {
            background: Some(pal::BG_OVERLAY.into()),
            border: border::rounded(pal::RADIUS_MD).color(pal::BORDER).width(1.0),
            ..Style::default()
        });
    let shadow_wrap = container(popup)
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
    let shadow_wrap = container(card)
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
