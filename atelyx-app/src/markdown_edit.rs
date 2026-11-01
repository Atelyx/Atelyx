//! Markdown 文本度量：行首行尾定位、排版字符空间换算与选区矩形（对话输入面共用）。

use iced::advanced::text as advanced_text;
use iced::advanced::text::Paragraph as _;
use iced::Rectangle;

use crate::markdown_view::is_cr;

/// `offset` 所在行的行首偏移（offset 非字符边界时向下取整）。
pub(crate) fn line_start_at(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[..offset].rfind('\n').map_or(0, |i| i + 1)
}

/// 自 `offset` 起本行行尾偏移（不含换行符）。
pub(crate) fn line_end_at(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[offset..].find('\n').map_or(text.len(), |i| offset + i)
}

/// 块内字节偏移 → 排版字符下标（`\r` 不占位，见 is_cr）。
pub(crate) fn char_index(content: &str, byte: usize) -> usize {
    let mut b = byte.min(content.len());
    while !content.is_char_boundary(b) {
        b -= 1;
    }
    visible_count(&content[..b])
}

/// 排版字符数：段落逐字符排版后的字符个数（`\r` 不占位）。
pub(crate) fn visible_count(content: &str) -> usize {
    content.chars().filter(|c| !is_cr(*c)).count()
}

pub(crate) type ParagraphT = <iced::Renderer as advanced_text::Renderer>::Paragraph;

/// 选区矩形：同行的字符矩形合并。
pub(crate) fn selection_rects(p: &ParagraphT, from: usize, to: usize) -> Vec<Rectangle> {
    let mut out: Vec<Rectangle> = Vec::new();
    for i in from..to {
        let Some(r) = p.span_bounds(i).into_iter().next() else {
            continue;
        };
        match out.last_mut() {
            Some(last) if (last.y - r.y).abs() < 0.5 => {
                last.width = (r.x + r.width) - last.x;
                last.height = last.height.max(r.height);
            }
            _ => out.push(r),
        }
    }
    out
}

/// 排版字符下标 → 内容内字节偏移（`\r` 不占位；越界归末尾）。
pub(crate) fn byte_of_char(content: &str, index: usize) -> usize {
    content
        .char_indices()
        .filter(|(_, c)| !is_cr(*c))
        .nth(index)
        .map_or(content.len(), |(b, _)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 排版字符空间：`\r` 不占位，字节偏移与字符下标在 CRLF 文本上仍互为反函数。
    #[test]
    fn crlf_stays_out_of_char_space() {
        let content = "甲\r\n乙";
        assert_eq!(visible_count(content), 3);
        // 甲(0..3) \r(3) \n(4) 乙(5..8)
        assert_eq!(char_index(content, 0), 0);
        assert_eq!(char_index(content, 3), 1);
        assert_eq!(char_index(content, 4), 1);
        assert_eq!(char_index(content, 5), 2);
        assert_eq!(char_index(content, content.len()), 3);
        assert_eq!(byte_of_char(content, 0), 0);
        assert_eq!(byte_of_char(content, 1), 4);
        assert_eq!(byte_of_char(content, 2), 5);
        assert_eq!(byte_of_char(content, 3), content.len());
    }
}
