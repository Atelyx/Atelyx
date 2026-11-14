//! 文字量算：实测宽（带缓存）、估算宽、省略号截断与贪心断行。

use std::cell::RefCell;
use std::collections::HashMap;

use iced::advanced::text::{self as advanced_text, Paragraph as _};
use iced::{Font, Renderer, Size};

use crate::font;

type TextParagraph = <Renderer as advanced_text::Renderer>::Paragraph;

/// 估算宽的单字符口径：ASCII ≈6.5px、其余 ≈13.5px（均按 12px 字号线性缩放）。
pub fn estimate_char_width(c: char, size: f32) -> f32 {
    let k = size / 12.0;
    if c.is_ascii() {
        6.5 * k
    } else {
        13.5 * k
    }
}

/// 估算文字宽：逐字累加 [estimate_char_width]。
/// 只用于命中判定与几何（估宽刻意高于实测，见调用方的槽位预留）；实际占位一律用 [measure]。
pub fn estimate_width(content: &str, size: f32) -> f32 {
    content.chars().map(|c| estimate_char_width(c, size)).sum()
}

/// 实测宽缓存上限（超出整体清空）。
const WIDTH_CACHE_CAP: usize = 8192;

/// 字宽缓存键：换字体族（epoch 变）、换字号、换字重/字形或换排版口径都必然换键。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WidthKey {
    epoch: u64,
    text: String,
    size_bits: u32,
    font: Font,
    /// false = 整串一次排版；true = 逐字符各自成 span（源码面口径）。
    per_char: bool,
}

thread_local! {
    /// 唯一一份单行实测宽表：表格自然宽、列表标记列宽、标题截断共用。
    static WIDTH_CACHE: RefCell<HashMap<WidthKey, f32>> = RefCell::new(HashMap::new());
}

/// 单行实测宽：与 iced 文本部件同字体字号 shaping（不换行）。
/// 按 (字体族 epoch, 文本, 字号位, 字体) 缓存；字体族变更后旧结果不可命中（epoch 在键里）。
pub fn measure(content: &str, size: f32, font: Font) -> f32 {
    measure_cached(content, size, font, false)
}

/// 逐字符 span 实测宽：每字各自成 span 排版，与源码面逐字符布局同口径。
/// 列表标记列宽靠它和源码面字宽一致（整串排版会带上字距/连字，列宽就对不齐）。
pub fn measure_per_char(content: &str, size: f32, font: Font) -> f32 {
    measure_cached(content, size, font, true)
}

fn width_key(content: &str, size: f32, font: Font, per_char: bool, epoch: u64) -> WidthKey {
    WidthKey { epoch, text: content.to_string(), size_bits: size.to_bits(), font, per_char }
}

fn measure_cached(content: &str, size: f32, font: Font, per_char: bool) -> f32 {
    let key = width_key(content, size, font, per_char, font::epoch());
    if let Some(width) = WIDTH_CACHE.with(|cache| cache.borrow().get(&key).copied()) {
        return width;
    }
    let width = shaped_width(content, size, font, per_char);
    WIDTH_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > WIDTH_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, width);
    });
    width
}

fn shaped_width(content: &str, size: f32, font: Font, per_char: bool) -> f32 {
    if per_char {
        let spans: Vec<advanced_text::Span<'_, (), Font>> = content
            .chars()
            .map(|ch| advanced_text::Span::new(ch.to_string()).font(font))
            .collect();
        TextParagraph::with_spans(advanced_text::Text {
            content: spans.as_slice(),
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: size.into(),
            // 单行量宽与行高无关；固定 1.0 让缓存键不必多带一个维度。
            line_height: advanced_text::LineHeight::Relative(1.0),
            font,
            align_x: advanced_text::Alignment::Left,
            align_y: iced::alignment::Vertical::Top,
            shaping: advanced_text::Shaping::Advanced,
            wrapping: advanced_text::Wrapping::None,
        })
        .min_bounds()
        .width
    } else {
        TextParagraph::with_text(advanced_text::Text {
            content,
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: size.into(),
            line_height: advanced_text::LineHeight::Relative(1.0),
            font,
            align_x: advanced_text::Alignment::Left,
            align_y: iced::alignment::Vertical::Top,
            shaping: advanced_text::Shaping::Auto,
            wrapping: advanced_text::Wrapping::None,
        })
        .min_bounds()
        .width
    }
}

/// 超宽截断并补省略号：返回能放下的最长前缀 + 省略号，文本本身放得下时原样返回。
/// 连省略号都放不下时仍返回省略号（零宽槽位也至少给一个省略号）。
///
/// measure 由调用方注入（估宽 / 实测宽各按自己的口径）；宽度随前缀增长单调时二分结果即最长前缀。
pub fn ellipsize(content: &str, max_width: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(content) <= max_width {
        return content.to_string();
    }
    let chars: Vec<char> = content.chars().collect();
    let candidate = |prefix_len: usize| {
        let mut candidate = String::with_capacity(content.len() + 3);
        candidate.extend(chars.iter().take(prefix_len));
        candidate.push('…');
        candidate
    };
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if measure(&candidate(mid)) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if measure(&candidate(lo)) <= max_width {
        candidate(lo)
    } else {
        "…".to_string()
    }
}

/// 按估宽截断的常用组合（[ellipsize] + [estimate_width]）。
pub fn ellipsize_estimated(content: &str, max_width: f32, size: f32) -> String {
    ellipsize(content, max_width, |s| estimate_width(s, size))
}

/// 按单元宽贪心断行：逐单元累加，放不下即起新行；单元本身超过整行时独占一行。
/// 返回每行文本与累计宽；空输入返回一行空串（调用方沿用「空行也占一行」的口径）。
pub fn greedy_wrap(cells: &[(char, f32)], max_width: f32) -> Vec<(String, f32)> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut width = 0.0f32;
    for &(cell, cell_width) in cells {
        if !line.is_empty() && width + cell_width > max_width {
            lines.push((std::mem::take(&mut line), width));
            width = 0.0;
        }
        line.push(cell);
        width += cell_width;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push((line, width));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定度量：每字符 1 单位（截断只按字符数收缩，与字体 shaping 解耦）。
    fn per_char(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn ellipsize_keeps_text_that_fits() {
        // 恰好等于上限：不截断
        assert_eq!(ellipsize("abc", 3.0, per_char), "abc");
        assert_eq!(ellipsize("", 0.0, per_char), "");
    }

    #[test]
    fn ellipsize_clips_at_last_fitting_char() {
        // 宽 4：保留 3 字 + 省略号（省略号自身占 1 单位）
        assert_eq!(ellipsize("abcdef", 4.0, per_char), "abc…");
        assert_eq!(ellipsize("abcdef", 3.0, per_char), "ab…");
    }

    #[test]
    fn ellipsize_mixed_cjk_and_latin() {
        // 中英混排按字符边界截断，不切半个字符
        assert_eq!(ellipsize("ab中文cd", 4.0, per_char), "ab中…");
        assert_eq!(ellipsize("中文测试", 3.0, per_char), "中文…");
        assert_eq!(ellipsize("abc中文测试", 3.0, per_char), "ab…");
    }

    #[test]
    fn ellipsize_returns_ellipsis_when_no_room() {
        // 连省略号都放不下：仍给省略号（零宽槽位也不返回空串）
        assert_eq!(ellipsize("abcdef", 0.0, per_char), "…");
        assert_eq!(ellipsize("abcdef", 0.5, per_char), "…");
        assert_eq!(ellipsize("abcdef", -1.0, per_char), "…");
    }

    #[test]
    fn ellipsize_estimated_respects_its_own_measure() {
        let size = 12.0;
        let shown = ellipsize_estimated("一个很长的文件名用来测估算宽.md", 80.0, size);
        assert!(shown.ends_with('…'));
        assert!(estimate_width(&shown, size) <= 80.0 + 0.01);
        // 单个省略号都放不下时仍返回省略号
        assert_eq!(ellipsize_estimated("abc", 1.0, size), "…");
    }

    #[test]
    fn greedy_wrap_breaks_on_width_and_keeps_order() {
        let cells: Vec<(char, f32)> = "abcdefg".chars().map(|c| (c, 1.0)).collect();
        let lines = greedy_wrap(&cells, 3.0);
        assert_eq!(
            lines.into_iter().map(|(text, _)| text).collect::<Vec<_>>(),
            vec!["abc", "def", "g"]
        );
    }

    #[test]
    fn greedy_wrap_lets_an_oversized_cell_stand_alone() {
        // 单个 cell 超宽时独占一行，不把下一字挤回本行，也不丢字
        let cells = vec![('中', 10.0), ('a', 1.0), ('b', 1.0)];
        let lines = greedy_wrap(&cells, 5.0);
        assert_eq!(lines, vec![("中".to_string(), 10.0), ("ab".to_string(), 2.0)]);
        let joined: String = lines.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(joined, "中ab");
    }

    #[test]
    fn greedy_wrap_empty_input_keeps_one_empty_line() {
        assert_eq!(greedy_wrap(&[], 10.0), vec![(String::new(), 0.0)]);
    }

    #[test]
    fn estimate_width_is_monotonic() {
        let size = 14.0;
        assert_eq!(estimate_width("", size), 0.0);
        let mut previous = 0.0;
        let mut prefix = String::new();
        for ch in "Atelyx 中文混排".chars() {
            prefix.push(ch);
            let width = estimate_width(&prefix, size);
            assert!(width >= previous, "加字不得变窄：{prefix:?}");
            previous = width;
        }
        // 同文同字号线性缩放，CJK 占宽大于 ASCII
        assert!(estimate_width("中", size) > estimate_width("a", size));
        assert!((estimate_width("ab", size * 2.0) - estimate_width("ab", size) * 2.0).abs() < 0.01);
    }

    #[test]
    fn width_cache_key_separates_epoch_size_font_and_shaping() {
        let font = Font::DEFAULT;
        let key = width_key("abc", 12.0, font, false, 1);
        assert_eq!(key, width_key("abc", 12.0, font, false, 1));
        // 换字体族（epoch 变）即换键，旧结果不可命中
        assert_ne!(key, width_key("abc", 12.0, font, false, 2));
        assert_ne!(key, width_key("abc", 13.0, font, false, 1));
        assert_ne!(key, width_key("abc", 12.0, font::bold(), false, 1));
        assert_ne!(key, width_key("abc", 12.0, font, true, 1));
        assert_ne!(key, width_key("abd", 12.0, font, false, 1));
    }
    /// 二分结果须与逐字收缩的线性实现对拍（估宽与实测宽两套口径各扫一遍字符边界）。
    #[test]
    fn ellipsize_matches_linear_reference() {
        fn linear(text: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> String {
            if measure(text) <= max_w {
                return text.to_string();
            }
            for (i, _) in text.char_indices().rev() {
                let candidate = format!("{}…", &text[..i]);
                if measure(&candidate) <= max_w {
                    return candidate;
                }
            }
            "…".to_string()
        }
        let samples = ["", "a", "abc", "中文测试", "ab中文cd", "长文本 mixed 中文 with spaces"];
        for text in samples {
            for max in (0..200).step_by(7).map(|v| v as f32 * 0.5) {
                let est = |s: &str| estimate_width(s, 12.0);
                assert_eq!(
                    ellipsize(text, max, est),
                    linear(text, max, est),
                    "估宽 {text:?} {max}"
                );
                let font = Font::DEFAULT;
                let measured = |s: &str| measure(s, 12.0, font);
                assert_eq!(
                    ellipsize(text, max, measured),
                    linear(text, max, measured),
                    "实测宽 {text:?} {max}"
                );
            }
        }
    }

    #[test]
    fn measured_width_is_repeatable_for_the_same_key() {
        // 只验同键复用；实测值随系统字体变化，不锁绝对值。
        let font = Font::DEFAULT;
        let first = measure("Atelyx", 14.0, font);
        assert_eq!(first, measure("Atelyx", 14.0, font));
        assert_eq!(measure("", 14.0, font), 0.0);
    }
}

