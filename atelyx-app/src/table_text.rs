//! 表格正文的断行测量：用编辑浮层同一套排版（iced 段落部件背后的 cosmic-text）逐行取段，
//! 行高、画布绘制与编辑浮层因此共用同一份行段。

use std::collections::HashMap;

use iced::advanced::graphics::text::{font_system, Paragraph, Version};
use iced::advanced::text::{self as advanced_text, Paragraph as _};
use iced::{alignment, Font, Size};

/// 正文行高倍数（画布绘制、断行测量与编辑浮层同源）。
pub const LINE_FACTOR: f32 = 1.45;

/// 绘制行：源文本切片与实测行宽（下划线与删除线取此宽）。
pub struct TextLine {
    pub text: String,
    pub width: f32,
}

/// 断行结果缓存：随布局重建滚动，只保留本轮用过的键。
#[derive(Default)]
pub struct WrapCache {
    entries: HashMap<WrapKey, WrapEntry>,
    /// 上次测量时的字体库版本（字体集会随系统变化，换库即作废旧行段）
    font_version: Option<Version>,
    pass: u64,
}

struct WrapEntry {
    pass: u64,
    lines: Vec<TextLine>,
}

/// 查找键：行高倍数与折行、字距口径本模块内固定，不入键；字号与换行宽按位取等。
#[derive(Debug, PartialEq, Eq, Hash)]
struct WrapKey {
    text: String,
    size_bits: u32,
    font: Font,
    wrap_bits: u32,
}

impl WrapKey {
    fn of(text: &str, size: f32, font: Font, wrap_w: f32) -> Self {
        WrapKey { text: text.to_string(), size_bits: size.to_bits(), font, wrap_bits: wrap_w.to_bits() }
    }
}

impl WrapCache {
    /// 开一轮布局重建。
    pub fn begin_pass(&mut self) {
        let version = font_system().read().expect("读取字体系统").version();
        if self.font_version != Some(version) {
            self.entries.clear();
            self.font_version = Some(version);
        }
        self.pass += 1;
    }

    /// 收一轮布局重建：本轮没再用到的键随之丢弃（列宽拖拽与正文改动留下的旧变体不累积）。
    pub fn end_pass(&mut self) {
        let pass = self.pass;
        self.entries.retain(|_, entry| entry.pass == pass);
    }

    /// 取整段正文的行段（无键则实测）。只能在布局重建里调：它要拿全局字体系统的写锁，
    /// 渲染通道内不可重入。
    pub fn lines(&mut self, text: &str, size: f32, font: Font, wrap_w: f32) -> &[TextLine] {
        let key = WrapKey::of(text, size, font, wrap_w);
        let pass = self.pass;
        let entry = self
            .entries
            .entry(key)
            .or_insert_with(|| WrapEntry { pass, lines: measure(text, size, font, wrap_w) });
        entry.pass = pass;
        &entry.lines
    }

    /// 只读取行段（绘制用）。
    pub fn peek(&self, text: &str, size: f32, font: Font, wrap_w: f32) -> Option<&[TextLine]> {
        self.entries.get(&WrapKey::of(text, size, font, wrap_w)).map(|entry| entry.lines.as_slice())
    }
}

/// 整段正文逐硬换行段实测的行段。
fn measure(text: &str, size: f32, font: Font, wrap_w: f32) -> Vec<TextLine> {
    text.split('\n')
        .flat_map(|segment| measure_segment(segment, size, font, wrap_w))
        .collect()
}

/// 在途草稿的行数：口径同 [`measure`]，但不写缓存——每次按键都是新文本，缓存只会累积草稿变体。
pub fn measure_live_lines(text: &str, size: f32, font: Font, wrap_w: f32) -> usize {
    text.split('\n')
        .map(|segment| layout(segment, size, font, wrap_w).buffer().layout_runs().count().max(1))
        .sum()
}

fn layout(segment: &str, size: f32, font: Font, wrap_w: f32) -> Paragraph {
    Paragraph::with_text(advanced_text::Text {
        content: segment,
        bounds: Size::new(wrap_w, f32::INFINITY),
        size: size.into(),
        line_height: advanced_text::LineHeight::Relative(LINE_FACTOR),
        font,
        // Default 而非 Left：Left 会让 iced 按实测宽收缩缓冲区重排，折行点随之漂移
        align_x: advanced_text::Alignment::Default,
        align_y: alignment::Vertical::Top,
        // 与编辑浮层一致：Advanced 字距；WordOrGlyph 折行使超长词退化为逐字形断，不溢出到邻列
        shaping: advanced_text::Shaping::Advanced,
        wrapping: advanced_text::Wrapping::WordOrGlyph,
    })
}

/// 单个硬换行段的排版结果：至少一行（空段也占一行高，与编辑浮层同口径）。
fn measure_segment(segment: &str, size: f32, font: Font, wrap_w: f32) -> Vec<TextLine> {
    let paragraph = layout(segment, size, font, wrap_w);
    let mut lines = Vec::new();
    for run in paragraph.buffer().layout_runs() {
        // 双向文（阿拉伯语 / 希伯来语混排）的字形序与字节序相反、行间还相互嵌套，
        // 取全体字形字节区间的两端夹出的才是这一行完整的连续区间
        let (from, to) = run
            .glyphs
            .iter()
            .fold((usize::MAX, 0usize), |acc, glyph| (acc.0.min(glyph.start), acc.1.max(glyph.end)));
        if from > to {
            lines.push(TextLine { text: String::new(), width: 0.0 });
            continue;
        }
        lines.push(TextLine { text: segment[from..to].to_string(), width: run.line_w });
    }
    if lines.is_empty() {
        lines.push(TextLine { text: String::new(), width: 0.0 });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_texts(text: &str, size: f32, wrap_w: f32) -> Vec<String> {
        measure(text, size, Font::DEFAULT, wrap_w).into_iter().map(|line| line.text).collect()
    }

    /// 硬换行逐段算行，空段也占一行。
    #[test]
    fn hard_breaks_count_as_lines() {
        assert_eq!(line_texts("a\n\nb", 14.0, f32::INFINITY), ["a", "", "b"]);
        assert_eq!(line_texts("", 14.0, f32::INFINITY), [""]);
    }

    /// 宽度足够时不按词断；窄到放不下一个词时退化逐字形断（画布不会把长词画到邻列）。
    #[test]
    fn long_word_breaks_instead_of_overflowing() {
        let word = "internationalization";
        assert_eq!(line_texts(word, 14.0, f32::INFINITY), [word]);
        assert!(line_texts(word, 14.0, 12.0).len() > 1);
    }

    /// 行段切片按字节连续铺满源串（切片落在字符边界，否则这里就 panic）。
    #[test]
    fn line_slices_tile_the_source() {
        let text = "多维表格的中文正文，带标点与混排abc词的断行。";
        let lines = line_texts(text, 14.0, 28.0);
        assert!(lines.len() > 1);
        assert_eq!(lines.concat(), text);
    }

    /// 在途草稿的行数与整段实测同口径（硬换行逐段、空段也占一行）。
    #[test]
    fn live_lines_match_measure() {
        let text = "第一行\n\n第二行很长的内容需要折行折行折行";
        for wrap_w in [f32::INFINITY, 24.0, 40.0] {
            assert_eq!(
                measure_live_lines(text, 14.0, Font::DEFAULT, wrap_w),
                measure(text, 14.0, Font::DEFAULT, wrap_w).len(),
            );
        }
    }

    /// 一轮重建收口后只留本轮用过的键；换行宽是键的一部分。
    #[test]
    fn pass_prunes_unused_keys() {
        let mut cache = WrapCache::default();
        cache.begin_pass();
        assert_eq!(cache.lines("正文", 14.0, Font::DEFAULT, f32::INFINITY).len(), 1);
        assert!(cache.peek("正文", 14.0, Font::DEFAULT, 40.0).is_none());
        cache.end_pass();

        cache.begin_pass();
        let _ = cache.lines("另一段", 14.0, Font::DEFAULT, f32::INFINITY);
        cache.end_pass();
        assert!(cache.peek("正文", 14.0, Font::DEFAULT, f32::INFINITY).is_none());
    }

    /// 双向文混排的行段不丢字、不重字：各行字节区间的并恰好按序铺满源文的非空白字符。
    #[test]
    fn bidi_lines_carry_every_character() {
        for text in ["עברית RTL mixed 中文", "مرحبا بالعالم mixed 中文"] {
            let lines = measure(text, 14.0, Font::DEFAULT, 60.0);
            let got: Vec<char> = lines
                .iter()
                .flat_map(|line| line.text.chars())
                .filter(|ch| !ch.is_whitespace())
                .collect();
            let want: Vec<char> = text.chars().filter(|ch| !ch.is_whitespace()).collect();
            assert_eq!(got, want, "行段与源文不一致：{:?}", text);
        }
    }
}
