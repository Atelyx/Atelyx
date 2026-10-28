//! 文字量算：与控件渲染同源的字体与段落度量，供宿主回答插件的测量请求。
//!
//! 画布按指令几何自绘，插件摆放文字前必须知道真实字宽——按字符数估算在中西文混排下必然错位。
//! 量算口径与 `text` 控件一致（字重映射、字体族、默认行高），因此量出来的占位就是渲染占位。

use atelyx_plugin_protocol::method::MeasureTextResult;
use atelyx_plugin_protocol::view::{TextFamily, TextWeight};
use iced::advanced::text::paragraph;
use iced::advanced::text::{Alignment, LineHeight, Shaping, Text, Wrapping};
use iced::alignment::Vertical;
use iced::{Font, Pixels, Renderer, Size};

type TextParagraph = <Renderer as iced::advanced::text::Renderer>::Paragraph;

/// 量算一段文字：`max_width` 给定时按该宽度换行，缺省按单行。
/// 返回的宽高为排版实际占位（换行时宽不超过 `max_width`），行数按默认行高折算。
pub fn measure_text(
    content: &str,
    size: f32,
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    max_width: Option<f32>,
) -> MeasureTextResult {
    let line_height = LineHeight::default().to_absolute(Pixels(size)).0;
    let wrapping = match max_width {
        Some(_) => Wrapping::Word,
        None => Wrapping::None,
    };
    let definition = Text {
        content,
        bounds: Size::new(max_width.unwrap_or(f32::INFINITY), f32::INFINITY),
        size: Pixels(size),
        line_height: LineHeight::default(),
        font: text_font(weight, family),
        align_x: Alignment::Default,
        align_y: Vertical::Top,
        shaping: Shaping::default(),
        wrapping,
    };
    let mut paragraph = paragraph::Plain::<TextParagraph>::default();
    paragraph.update(definition);
    let measured = paragraph.min_bounds();
    // 空文本的排版高度为 0：行数按 1 计，与「一个空行」的实际占位一致
    let lines = if measured.height <= 0.0 || line_height <= 0.0 {
        1
    } else {
        (measured.height / line_height).round().max(1.0) as u32
    };
    MeasureTextResult { width: measured.width, height: measured.height, lines }
}

/// 字体族与字重 → 字体（等宽族始终取系统等宽字体，不随界面字体族设置变化）。
pub fn text_font(weight: Option<TextWeight>, family: Option<TextFamily>) -> Font {
    let base = match family {
        Some(TextFamily::Mono) => atelyx_ui::font::mono(),
        _ => atelyx_ui::regular(),
    };
    Font {
        weight: weight.map(weight_of).unwrap_or(iced::font::Weight::Normal),
        ..base
    }
}

fn weight_of(weight: TextWeight) -> iced::font::Weight {
    match weight {
        TextWeight::Normal => iced::font::Weight::Normal,
        TextWeight::Medium => iced::font::Weight::Medium,
        TextWeight::SemiBold => iced::font::Weight::Semibold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_width_grows_with_content_and_size() {
        let short = measure_text("ab", 14.0, None, None, None);
        let long = measure_text("abcdef", 14.0, None, None, None);
        let larger = measure_text("ab", 28.0, None, None, None);
        assert_eq!(short.lines, 1);
        assert!(long.width > short.width, "字多则更宽");
        assert!(larger.width > short.width, "字号大则更宽");
        assert!(larger.height > short.height, "字号大则更高");
    }

    #[test]
    fn max_width_wraps_and_respects_the_limit() {
        let text = "把这段足够长的中文按较窄的宽度换行之后再量一次尺寸";
        let single = measure_text(text, 14.0, None, None, None);
        let wrapped = measure_text(text, 14.0, None, None, Some(60.0));
        assert!(single.lines == 1);
        assert!(wrapped.lines > 1, "窄宽度下应换行：{wrapped:?}");
        assert!(wrapped.width <= 60.5, "换行后宽度不得超过给定上限：{wrapped:?}");
        assert!(wrapped.height > single.height);
    }

    #[test]
    fn empty_text_still_takes_one_line() {
        let empty = measure_text("", 14.0, None, None, Some(100.0));
        assert_eq!(empty.lines, 1);
        assert_eq!(empty.width, 0.0);
    }

    #[test]
    fn mono_and_weight_change_the_font() {
        let ui = text_font(None, None);
        let mono = text_font(None, Some(TextFamily::Mono));
        assert!(matches!(mono.family, iced::font::Family::Monospace));
        assert_ne!(ui, mono);
        assert_eq!(
            text_font(Some(TextWeight::SemiBold), None).weight,
            iced::font::Weight::Semibold
        );
    }
}
