//! 插件面的宿主侧界面资源：文字量算与主题取色。
//! 量算口径与预置插件视图同一套字体与行高：插件按它摆放，量出的占位就是宿主渲染的占位。
//! 主题取色把协议颜色 token 映射到当前调色板，插件与宿主观感不漂移。

use atelyx_core::plugin_runtime::{
    ColorToken, FontSizes, MeasureSegment, MeasureTextResult, Radii, TextFamily, TextLayout,
    TextShaping, TextWeight, TextWrap, ThemeColors, ThemeResult,
};
use iced::advanced::text::paragraph;
use iced::advanced::text::{Alignment, LineHeight, Shaping, Text, Wrapping};
use iced::alignment::Vertical;
use iced::{Color, Font, Pixels, Renderer, Size};

use crate::ui::palette as pal;

type TextParagraph = <Renderer as iced::advanced::text::Renderer>::Paragraph;

/// 未声明行高时的相对行高倍数（界面文本与插件帧文字同一档）。
pub const DEFAULT_LINE_HEIGHT: f32 = 1.2;

/// 量算一段文字：`max_width` 给定时按该宽度换行，缺省按单行。
/// 返回的宽高为排版实际占位（换行时宽不超过 `max_width`），行数按实际行高折算。
/// `layout` 与绘制指令的排版口径同源；声明 `segments` 时按行给出行段（逐行绘制的调用方用）。
pub fn measure_text(
    content: &str,
    size: f32,
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    family_name: Option<&str>,
    max_width: Option<f32>,
    layout: Option<TextLayout>,
    segments: bool,
) -> MeasureTextResult {
    let factor = layout
        .and_then(|layout| layout.line_height)
        .unwrap_or(DEFAULT_LINE_HEIGHT)
        .max(0.01);
    let line_height = size * factor;
    let wrapping = match (max_width, layout.and_then(|layout| layout.wrap)) {
        (None, _) => Wrapping::None,
        (Some(_), Some(TextWrap::Word)) => Wrapping::Word,
        (Some(_), Some(TextWrap::WordOrGlyph)) => Wrapping::WordOrGlyph,
        (Some(_), None) => Wrapping::Word,
    };
    let shaping = match layout.and_then(|layout| layout.shaping) {
        Some(TextShaping::Auto) => Shaping::Auto,
        Some(TextShaping::Basic) => Shaping::Basic,
        Some(TextShaping::Advanced) => Shaping::Advanced,
        None => Shaping::default(),
    };
    let definition = Text {
        content,
        bounds: Size::new(max_width.unwrap_or(f32::INFINITY), f32::INFINITY),
        size: Pixels(size),
        line_height: LineHeight::Relative(factor),
        font: text_font_named(weight, family, family_name),
        align_x: Alignment::Default,
        align_y: Vertical::Top,
        shaping,
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
    let segments = segments.then(|| {
        line_segments(content, size, weight, family, family_name, max_width, factor, shaping, wrapping)
    });
    MeasureTextResult { width: measured.width, height: measured.height, lines, segments }
}

/// 按硬换行与折行逐行取行段：每行给源文本切片与实测行宽。
/// 取法与表格正文的断行同口径（按字形字节区间的两端夹出整行区间，bidi 混排也不漏字）。
#[allow(clippy::too_many_arguments)]
fn line_segments(
    content: &str,
    size: f32,
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    family_name: Option<&str>,
    max_width: Option<f32>,
    factor: f32,
    shaping: Shaping,
    wrapping: Wrapping,
) -> Vec<MeasureSegment> {
    let mut out = Vec::new();
    for segment in content.split('\n') {
        let definition = Text {
            content: segment,
            bounds: Size::new(max_width.unwrap_or(f32::INFINITY), f32::INFINITY),
            size: Pixels(size),
            line_height: LineHeight::Relative(factor),
            font: text_font_named(weight, family, family_name),
            align_x: Alignment::Default,
            align_y: Vertical::Top,
            shaping,
            wrapping,
        };
        let mut paragraph = paragraph::Plain::<TextParagraph>::default();
        paragraph.update(definition);
        let buffer = paragraph.raw().buffer();
        let mut emitted = 0usize;
        for run in buffer.layout_runs() {
            let mut low = usize::MAX;
            let mut high = 0usize;
            for glyph in run.glyphs {
                low = low.min(glyph.start);
                high = high.max(glyph.end);
            }
            if low == usize::MAX || low >= segment.len() {
                continue;
            }
            let end = high.min(segment.len());
            let text = segment[low..end].to_string();
            out.push(MeasureSegment { text, width: run.line_w });
            emitted += 1;
        }
        // 空段也占一行（与「一个空行」的实际占位一致）
        if emitted == 0 {
            out.push(MeasureSegment { text: String::new(), width: 0.0 });
        }
    }
    out
}

/// 字体族与字重 → 字体：给了显式族名且该族已安装时按名取，否则按 `family` 口径回落。
pub fn text_font_named(
    weight: Option<TextWeight>,
    family: Option<TextFamily>,
    family_name: Option<&str>,
) -> Font {
    if let Some(name) = family_name {
        // 只有已安装族才驻留成 `&'static str`（未安装的族名不驻留，免任意字符串漏进字体族）
        if let Some(resident) = crate::ui::font::lookup(name) {
            return Font {
                weight: weight.map(weight_of).unwrap_or(iced::font::Weight::Normal),
                family: iced::font::Family::Name(resident),
                ..Font::default()
            };
        }
    }
    text_font(weight, family)
}

/// 字体族与字重 → 字体（等宽族始终取系统等宽字体，不随界面字体族设置变化）。
pub fn text_font(weight: Option<TextWeight>, family: Option<TextFamily>) -> Font {
    let base = match family {
        Some(TextFamily::Mono) => crate::ui::font::mono(),
        _ => crate::ui::regular(),
    };
    Font { weight: weight.map(weight_of).unwrap_or(iced::font::Weight::Normal), ..base }
}

fn weight_of(weight: TextWeight) -> iced::font::Weight {
    match weight {
        TextWeight::Normal => iced::font::Weight::Normal,
        TextWeight::Medium => iced::font::Weight::Medium,
        TextWeight::SemiBold => iced::font::Weight::Semibold,
        TextWeight::Bold => iced::font::Weight::Bold,
    }
}

/// 主题 token → 当前调色板颜色。
pub fn token_color(token: ColorToken) -> Color {
    match token {
        ColorToken::Text => pal::TEXT_PRIMARY,
        ColorToken::TextSecondary => pal::TEXT_SECONDARY,
        ColorToken::TextMuted => pal::TEXT_MUTED,
        ColorToken::Accent => pal::ACCENT,
        ColorToken::AccentSoft => pal::ACCENT_SOFT,
        ColorToken::Danger => pal::DANGER,
        ColorToken::DangerSoft => pal::DANGER_SOFT,
        ColorToken::Success => pal::SUCCESS,
        ColorToken::SuccessSoft => pal::SUCCESS_SOFT,
        ColorToken::Warning => pal::WARNING,
        ColorToken::WarningSoft => pal::WARNING_SOFT,
        ColorToken::Surface => pal::BG_TERTIARY,
        ColorToken::SurfaceMuted => pal::BG_SECONDARY,
        ColorToken::SurfaceSunken => pal::BG_SUNKEN,
        ColorToken::Hover => pal::HOVER,
        ColorToken::Border => pal::BORDER,
        ColorToken::BorderStrong => pal::BORDER_STRONG,
        ColorToken::Background => pal::BG_PRIMARY,
        ColorToken::BorderSubtle => pal::BORDER_SUBTLE,
        ColorToken::Selection => pal::SELECTION,
        ColorToken::Scrim => pal::SCRIM,
        ColorToken::Transparent => Color::TRANSPARENT,
        ColorToken::SurfaceOverlay => pal::BG_OVERLAY,
    }
}

/// 主题查询结果：颜色取界面 token（十六进制导出）、字号档与圆角档取界面常量、事件色板同调色板。
pub fn theme_result() -> ThemeResult {
    fn hex(color: Color) -> String {
        let [r, g, b, a] = color.into_rgba8();
        if a == u8::MAX {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    ThemeResult {
        colors: ThemeColors {
            text: hex(token_color(ColorToken::Text)),
            text_secondary: hex(token_color(ColorToken::TextSecondary)),
            text_muted: hex(token_color(ColorToken::TextMuted)),
            accent: hex(token_color(ColorToken::Accent)),
            accent_soft: hex(token_color(ColorToken::AccentSoft)),
            danger: hex(token_color(ColorToken::Danger)),
            danger_soft: hex(token_color(ColorToken::DangerSoft)),
            success: hex(token_color(ColorToken::Success)),
            success_soft: hex(token_color(ColorToken::SuccessSoft)),
            warning: hex(token_color(ColorToken::Warning)),
            warning_soft: hex(token_color(ColorToken::WarningSoft)),
            surface: hex(token_color(ColorToken::Surface)),
            surface_muted: hex(token_color(ColorToken::SurfaceMuted)),
            surface_sunken: hex(token_color(ColorToken::SurfaceSunken)),
            hover: hex(token_color(ColorToken::Hover)),
            border: hex(token_color(ColorToken::Border)),
            border_strong: hex(token_color(ColorToken::BorderStrong)),
            background: hex(token_color(ColorToken::Background)),
            border_subtle: hex(token_color(ColorToken::BorderSubtle)),
            selection: hex(token_color(ColorToken::Selection)),
            scrim: hex(token_color(ColorToken::Scrim)),
            transparent: hex(token_color(ColorToken::Transparent)),
        },
        font_sizes: FontSizes {
            micro: pal::fs_micro(),
            caption: pal::fs_caption(),
            ui: pal::fs_ui(),
            body: pal::fs_body(),
            h2: pal::fs_h2(),
            h1: pal::fs_h1(),
        },
        radii: Radii { xs: pal::RADIUS_XS, sm: pal::RADIUS_SM, md: pal::RADIUS_MD },
        event_colors: pal::EVENT_COLORS.iter().map(|color| hex(*color)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_width_grows_with_content_and_size() {
        let short = measure_text("ab", 14.0, None, None, None, None, None, false);
        let long = measure_text("abcdef", 14.0, None, None, None, None, None, false);
        let larger = measure_text("ab", 28.0, None, None, None, None, None, false);
        assert_eq!(short.lines, 1);
        assert!(long.width > short.width, "字多则更宽");
        assert!(larger.width > short.width, "字号大则更宽");
        assert!(larger.height > short.height, "字号大则更高");
    }

    #[test]
    fn max_width_wraps_and_respects_the_limit() {
        let text = "把这段足够长的中文按较窄的宽度换行之后再量一次尺寸";
        let single = measure_text(text, 14.0, None, None, None, None, None, false);
        let wrapped = measure_text(text, 14.0, None, None, None, Some(60.0), None, false);
        assert!(single.lines == 1);
        assert!(wrapped.lines > 1, "窄宽度下应换行：{wrapped:?}");
        assert!(wrapped.width <= 60.5, "换行后宽度不得超过给定上限：{wrapped:?}");
        assert!(wrapped.height > single.height);
    }

    #[test]
    fn empty_text_still_takes_one_line() {
        let empty = measure_text("", 14.0, None, None, None, Some(100.0), None, false);
        assert_eq!(empty.lines, 1);
        assert_eq!(empty.width, 0.0);
    }

    #[test]
    fn mono_and_weight_change_the_font() {
        let ui = text_font(None, None);
        let mono = text_font(None, Some(TextFamily::Mono));
        assert!(matches!(mono.family, iced::font::Family::Monospace));
        assert_ne!(ui, mono);
        assert_eq!(text_font(Some(TextWeight::SemiBold), None).weight, iced::font::Weight::Semibold);
    }

    /// 主题表与 token 取色同源：十六进制导出必须就是渲染用的那个颜色。
    #[test]
    fn theme_table_matches_token_colors() {
        let theme = theme_result();
        let [r, g, b, _] = token_color(ColorToken::Accent).into_rgba8();
        assert_eq!(theme.colors.accent, format!("#{r:02x}{g:02x}{b:02x}"));
        assert_eq!(theme.colors.transparent, "#00000000");
        // 事件色板与界面同序：插件按索引取色即宿主日程色
        assert_eq!(theme.event_colors.len(), pal::EVENT_COLORS.len());
        let [r, g, b, _] = pal::EVENT_COLORS[0].into_rgba8();
        assert_eq!(theme.event_colors[0], format!("#{r:02x}{g:02x}{b:02x}"));
    }
}
