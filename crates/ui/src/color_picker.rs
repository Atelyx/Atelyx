//! 取色板：预设色块 + HSV 取色（明度/饱和度方块 + 色相条）+ 十六进制输入。
//! 只负责渲染与事件上抛；当前色、预览与提交语义由调用方持有。

use iced::widget::canvas::{self, Canvas, Frame, Path, Stroke};
use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{
    alignment, border, mouse, Color, Element, Length, Padding, Point, Rectangle, Size, Theme,
};

use super::controls;
use super::palette as pal;
use super::regular;

/// 取色板弹层宽度（预设色块网格与取色区同宽）。
pub const PANEL_W: f32 = 208.0;

const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 8.0;
const SWATCH: f32 = 28.0;
const SWATCH_GAP: f32 = 8.0;
const GRID_COLS: usize = 5;
const SV_H: f32 = 108.0;
const HUE_H: f32 = 12.0;
const AREA_GAP: f32 = 8.0;
const ITEM_GAP: f32 = 10.0;
const ROW_H: f32 = 28.0;
const HUE_Y: f32 = SV_H + AREA_GAP;

/// HSV 颜色（色相 0–360，饱和度 / 明度 0–1）。保留色相是取色器在纯黑/纯白处仍能续调的前提。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Default for Hsv {
    fn default() -> Self {
        Self { h: 0.0, s: 1.0, v: 1.0 }
    }
}

impl Hsv {
    /// 由 sRGB 颜色取 HSV（灰阶色相为 0）。
    pub fn from_color(color: Color) -> Self {
        let (r, g, b) = (color.r, color.g, color.b);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let h = if delta <= f32::EPSILON {
            0.0
        } else if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        Self {
            h,
            s: if max <= f32::EPSILON { 0.0 } else { delta / max },
            v: max,
        }
    }

    /// 转 sRGB（alpha 恒为 1）。
    pub fn to_color(self) -> Color {
        let h = self.h.rem_euclid(360.0) / 60.0;
        let s = self.s.clamp(0.0, 1.0);
        let v = self.v.clamp(0.0, 1.0);
        let c = v * s;
        let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match h as u8 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = v - c;
        Color::from_rgb(r + m, g + m, b + m)
    }

    /// 规范化十六进制串（小写 `#rrggbb`）。
    pub fn hex(self) -> String {
        format_hex(self.to_color())
    }
}

/// 十六进制色值：`#RRGGBB` / `#RGB`（`#` 可省略，大小写不敏感）。
pub fn parse_hex(text: &str) -> Option<Color> {
    let s = text.trim().trim_start_matches('#');
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (r, g, b) = match s.len() {
        3 => (
            u8::from_str_radix(&s[0..1].repeat(2), 16).ok()?,
            u8::from_str_radix(&s[1..2].repeat(2), 16).ok()?,
            u8::from_str_radix(&s[2..3].repeat(2), 16).ok()?,
        ),
        6 => (
            u8::from_str_radix(&s[0..2], 16).ok()?,
            u8::from_str_radix(&s[2..4], 16).ok()?,
            u8::from_str_radix(&s[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(Color::from_rgb8(r, g, b))
}

/// 规范十六进制串（小写 `#rrggbb`）。
pub fn format_hex(color: Color) -> String {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", channel(color.r), channel(color.g), channel(color.b))
}

/// 取色板内容：预设色块 / 取色区 / 十六进制输入 / 可选「默认」动作。
/// 返回元素与其估算高度（弹层翻转与钳制按此高度判定）。
#[allow(clippy::too_many_arguments)]
pub fn panel<'a, M: Clone + 'a>(
    presets: &[&str],
    current: Option<Color>,
    hsv: Hsv,
    hex: &str,
    reset: Option<(&'static str, M)>,
    submit: M,
    on_preview: impl Fn(Hsv) -> M + 'a,
    on_commit: impl Fn(Hsv) -> M + 'a,
    on_draft: impl Fn(String) -> M + 'a,
) -> (Element<'a, M>, f32) {
    let picks: Vec<(Color, M)> = presets
        .iter()
        .filter_map(|raw| parse_hex(raw))
        .map(|color| (color, on_commit(Hsv::from_color(color))))
        .collect();
    let rows_n = picks.len().div_ceil(GRID_COLS).max(1);
    let grid_h = rows_n as f32 * SWATCH + (rows_n.saturating_sub(1)) as f32 * SWATCH_GAP;

    let mut grid = column![].spacing(SWATCH_GAP);
    for chunk in picks.chunks(GRID_COLS) {
        let mut line = row![].spacing(SWATCH_GAP);
        for (color, pick) in chunk {
            line = line.push(swatch(*color, current == Some(*color), pick.clone()));
        }
        grid = grid.push(line);
    }

    let area_w = PANEL_W - PAD_X * 2.0;
    let canvas: Element<'a, M> = Canvas::new(Picker {
        hsv,
        preview: Box::new(on_preview),
        commit: Box::new(on_commit),
    })
    .width(Length::Fixed(area_w))
    .height(Length::Fixed(HUE_Y + HUE_H))
    .into();

    let preview_color = parse_hex(hex).unwrap_or_else(|| hsv.to_color());
    let chip = container(space())
        .width(Length::Fixed(24.0))
        .height(Length::Fixed(24.0))
        .style(move |_t| container::Style {
            background: Some(preview_color.into()),
            border: border::rounded(pal::RADIUS_XS).color(pal::BORDER_STRONG).width(1.0),
            ..container::Style::default()
        });
    let invalid = !hex.trim().is_empty() && parse_hex(hex).is_none();
    let input = text_input("十六进制颜色", hex)
        .size(pal::fs_caption())
        .font(regular())
        .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
        .width(Length::Fill)
        .on_input(on_draft)
        .on_submit(submit)
        .style(move |_t, status| {
            let mut style = controls::form_input_style(status);
            if invalid {
                style.border = border::rounded(pal::RADIUS_SM).color(pal::DANGER).width(1.0);
            }
            style
        });
    let hex_row = row![chip, input]
        .spacing(8.0)
        .align_y(alignment::Vertical::Center);

    let mut content = column![grid, canvas, hex_row].spacing(ITEM_GAP);
    let mut height = PAD_Y * 2.0 + grid_h + ITEM_GAP * 2.0 + HUE_Y + HUE_H + ROW_H;
    if let Some((label, message)) = reset {
        content = content.push(
            button(text(label).size(pal::fs_caption()).color(pal::TEXT_PRIMARY))
                .width(Length::Fill)
                .padding(Padding::ZERO.horizontal(8.0).vertical(6.0))
                .style(controls::row_style(false, pal::RADIUS_SM))
                .on_press(message),
        );
        height += ITEM_GAP + ROW_H;
    }

    let element = container(content)
        .width(Length::Fill)
        .padding(Padding::ZERO.horizontal(PAD_X).vertical(PAD_Y))
        .into();
    (element, height)
}

/// 预设色块按钮：当前色带强调描边。
fn swatch<'a, M: Clone + 'a>(color: Color, selected: bool, on_press: M) -> Element<'a, M> {
    let block = container(space())
        .width(Length::Fixed(SWATCH))
        .height(Length::Fixed(SWATCH))
        .style(move |_t| container::Style {
            background: Some(color.into()),
            border: border::rounded(pal::RADIUS_XS)
                .color(if selected { pal::ACCENT } else { pal::BORDER_STRONG })
                .width(if selected { 2.0 } else { 1.0 }),
            ..container::Style::default()
        });
    button(block)
        .padding(0.0)
        .style(|_t, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                .then_some(pal::HOVER.into()),
            border: border::rounded(pal::RADIUS_XS),
            ..button::Style::default()
        })
        .on_press(on_press)
        .into()
}

/// 取色画布：按下选定区域，拖动上抛预览，松手提交一次。
struct Picker<'a, M> {
    hsv: Hsv,
    preview: Box<dyn Fn(Hsv) -> M + 'a>,
    commit: Box<dyn Fn(Hsv) -> M + 'a>,
}

#[derive(Clone, Copy, Default)]
struct PickerState {
    drag: Option<Drag>,
}

#[derive(Clone, Copy)]
struct Drag {
    area: Area,
    last: Point,
}

#[derive(Clone, Copy)]
enum Area {
    Sv,
    Hue,
}

impl<'a, M: Clone + 'a> canvas::Program<M> for Picker<'a, M> {
    type State = PickerState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<iced::widget::Action<M>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                let Some(area) = area_at(point) else {
                    return Some(iced::widget::Action::capture());
                };
                state.drag = Some(Drag { area, last: point });
                let next = self.picked(area, point, bounds.size());
                Some(iced::widget::Action::publish((self.preview)(next)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let drag = state.drag.as_mut()?;
                // 拖出画布仍按边缘取色：窗口坐标换算回画布本地再钳制
                let point = cursor
                    .position_in(bounds)
                    .or_else(|| cursor.position().map(|p| Point::new(p.x - bounds.x, p.y - bounds.y)))
                    .unwrap_or(drag.last);
                drag.last = point;
                let next = self.picked(drag.area, point, bounds.size());
                Some(iced::widget::Action::publish((self.preview)(next)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let drag = state.drag.take()?;
                let next = self.picked(drag.area, drag.last, bounds.size());
                Some(iced::widget::Action::publish((self.commit)(next)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        draw_picker(&mut frame, bounds.size(), self.hsv);
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.drag.is_some() {
            return mouse::Interaction::Grabbing;
        }
        match cursor.position_in(bounds).and_then(area_at) {
            Some(_) => mouse::Interaction::Crosshair,
            None => mouse::Interaction::default(),
        }
    }
}

impl<'a, M> Picker<'a, M> {
    fn picked(&self, area: Area, point: Point, size: Size) -> Hsv {
        match area {
            Area::Sv => Hsv {
                h: self.hsv.h,
                s: (point.x / size.width.max(1.0)).clamp(0.0, 1.0),
                v: (1.0 - point.y / SV_H).clamp(0.0, 1.0),
            },
            Area::Hue => Hsv {
                h: (point.x / size.width.max(1.0)).clamp(0.0, 1.0) * 360.0,
                s: self.hsv.s,
                v: self.hsv.v,
            },
        }
    }
}

fn area_at(point: Point) -> Option<Area> {
    if (0.0..=SV_H).contains(&point.y) {
        Some(Area::Sv)
    } else if (HUE_Y..=HUE_Y + HUE_H).contains(&point.y) {
        Some(Area::Hue)
    } else {
        None
    }
}

fn draw_picker(frame: &mut Frame, size: Size, hsv: Hsv) {
    let sv = Size::new(size.width, SV_H);
    // 明度/饱和度方块 = 白→当前色相（横向）+ 黑透明度 0→1（纵向），等价于 HSV 取色公式。
    frame.fill_rectangle(
        Point::ORIGIN,
        sv,
        canvas::gradient::Linear::new(Point::ORIGIN, Point::new(size.width, 0.0))
            .add_stop(0.0, Color::WHITE)
            .add_stop(1.0, Hsv { s: 1.0, v: 1.0, ..hsv }.to_color()),
    );
    frame.fill_rectangle(
        Point::ORIGIN,
        sv,
        canvas::gradient::Linear::new(Point::ORIGIN, Point::new(0.0, SV_H))
            .add_stop(0.0, Color::BLACK.scale_alpha(0.0))
            .add_stop(1.0, Color::BLACK),
    );

    let mut hues = canvas::gradient::Linear::new(
        Point::new(0.0, HUE_Y),
        Point::new(size.width, HUE_Y),
    );
    for i in 0..=6 {
        let hue = i as f32 * 60.0;
        hues = hues.add_stop(i as f32 / 6.0, Hsv { h: hue, s: 1.0, v: 1.0 }.to_color());
    }
    frame.fill_rectangle(
        Point::new(0.0, HUE_Y),
        Size::new(size.width, HUE_H),
        hues,
    );

    let border = Stroke {
        style: canvas::Style::Solid(pal::BORDER_STRONG),
        width: 1.0,
        ..Stroke::default()
    };
    frame.stroke_rectangle(Point::ORIGIN, sv, border);
    frame.stroke_rectangle(Point::new(0.0, HUE_Y), Size::new(size.width, HUE_H), border);

    let center = Point::new(
        hsv.s.clamp(0.0, 1.0) * size.width,
        (1.0 - hsv.v.clamp(0.0, 1.0)) * SV_H,
    );
    frame.stroke(
        &Path::circle(center, 7.0),
        Stroke {
            style: canvas::Style::Solid(Color::BLACK.scale_alpha(0.65)),
            width: 3.0,
            ..Stroke::default()
        },
    );
    frame.stroke(
        &Path::circle(center, 5.0),
        Stroke {
            style: canvas::Style::Solid(Color::WHITE),
            width: 2.0,
            ..Stroke::default()
        },
    );

    let x = (hsv.h.clamp(0.0, 360.0) / 360.0 * size.width).min(size.width);
    let line = Path::line(Point::new(x, HUE_Y), Point::new(x, HUE_Y + HUE_H));
    frame.stroke(
        &line,
        Stroke {
            style: canvas::Style::Solid(Color::BLACK.scale_alpha(0.65)),
            width: 5.0,
            ..Stroke::default()
        },
    );
    frame.stroke(
        &line,
        Stroke {
            style: canvas::Style::Solid(Color::WHITE),
            width: 2.0,
            ..Stroke::default()
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_accepts_optional_hash_and_short_form() {
        assert_eq!(parse_hex("#e05252"), Some(Color::from_rgb8(0xe0, 0x52, 0x52)));
        assert_eq!(parse_hex("E05252"), Some(Color::from_rgb8(0xe0, 0x52, 0x52)));
        assert_eq!(parse_hex("#f00"), Some(Color::from_rgb8(0xff, 0x00, 0x00)));
        assert_eq!(parse_hex(" #0f0 "), Some(Color::from_rgb8(0x00, 0xff, 0x00)));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#ggghhh"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn hex_round_trips_through_u8_channels() {
        let color = Color::from_rgb8(0x4f, 0x8f, 0xd0);
        assert_eq!(format_hex(color), "#4f8fd0");
        assert_eq!(parse_hex(&format_hex(color)), Some(color));
    }

    #[test]
    fn hsv_conversion_round_trips_primaries_and_grays() {
        for rgb in [
            Color::from_rgb8(0xe0, 0x52, 0x52),
            Color::from_rgb8(0x4f, 0xae, 0x6a),
            Color::from_rgb8(0x4f, 0x8f, 0xd0),
            Color::from_rgb8(0xff, 0xff, 0xff),
            Color::from_rgb8(0x00, 0x00, 0x00),
            Color::from_rgb8(0x8a, 0x8a, 0x8a),
        ] {
            let back = Hsv::from_color(rgb).to_color();
            assert_eq!(format_hex(back), format_hex(rgb));
        }
    }

    #[test]
    fn zero_saturation_or_value_keeps_hue_for_continued_adjustment() {
        let hsv = Hsv { h: 210.0, s: 0.0, v: 0.0 };
        assert_eq!(format_hex(hsv.to_color()), "#000000");
        assert_eq!(format_hex(Hsv { s: 0.0, v: 1.0, ..hsv }.to_color()), "#ffffff");
        assert_eq!(format_hex(Hsv { s: 1.0, v: 1.0, ..hsv }.to_color()), "#0080ff");
    }
}
