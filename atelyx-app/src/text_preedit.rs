//! 输入法组合串的排版口径：组合串作为光标处的临时字符进入段落，其后文本随排版推开。

use std::borrow::Cow;
use std::ops::Range;

use crate::markdown_view::is_cr;

/// 组合串的位置：插入点（排版字符下标）与串长（字符数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composed {
    insert: usize,
    len: usize,
}

impl Composed {
    /// `caret` 为插入点（排版字符下标，`\r` 不占位）；空串视同未组合。
    pub(crate) fn new(caret: usize, preedit: Option<&str>) -> Self {
        Self { insert: caret, len: preedit.map_or(0, |p| p.chars().count()) }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.len > 0
    }

    /// 组合串在排版文本中的字符区间。
    pub(crate) fn span(&self) -> Range<usize> {
        self.insert..self.insert + self.len
    }

    /// 正文字符下标 → 排版字符下标（插入点及之后整体后移组合串长度）。
    pub(crate) fn index(&self, idx: usize) -> usize {
        if idx <= self.insert {
            idx
        } else {
            idx + self.len
        }
    }

    /// 光标位的排版字符下标：插入点及之后落在组合串尾（选区间仍用 [`Self::index`]）。
    pub(crate) fn caret(&self, idx: usize) -> usize {
        if idx < self.insert {
            idx
        } else {
            idx + self.len
        }
    }
}

/// 排版文本：组合串插在 `at.insert` 处；未组合时原样借用。
/// 插入点是排版字符下标（`\r` 不占位）：落在换行之前时退到行尾风格之前（`\r\n` 不可拆开）。
pub(crate) fn compose<'a>(text: &'a str, at: Composed, preedit: Option<&str>) -> Cow<'a, str> {
    let Some(pre) = preedit.filter(|p| !p.is_empty()) else {
        return Cow::Borrowed(text);
    };
    let mut split = text
        .char_indices()
        .filter(|(_, c)| !is_cr(*c))
        .nth(at.insert)
        .map_or(text.len(), |(b, _)| b);
    if text[split..].starts_with('\n') && split > 0 && text.as_bytes()[split - 1] == b'\r' {
        split -= 1;
    }
    let mut out = String::with_capacity(text.len() + pre.len());
    out.push_str(&text[..split]);
    out.push_str(pre);
    out.push_str(&text[split..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_inserts_preedit_at_caret() {
        let at = Composed::new(3, Some("ni"));
        assert_eq!(compose("abcdef", at, Some("ni")).as_ref(), "abcnidef");
        assert_eq!(compose("abc", at, None).as_ref(), "abc");
    }

    /// 插入点是排版字符下标：CRLF 文本里 `\r` 不占位，组合串不得插进 `\r` 与 `\n` 之间。
    #[test]
    fn compose_places_preedit_after_crlf() {
        // 行首插入（插入点指向第二行的首个可见字符）
        let at = Composed::new(2, Some("ni"));
        assert_eq!(compose("甲\r\n乙", at, Some("ni")).as_ref(), "甲\r\nni乙");
        // 行尾插入（插入点指向换行）：落在 `\r\n` 之前
        let at = Composed::new(1, Some("ni"));
        assert_eq!(compose("甲\r\n乙", at, Some("ni")).as_ref(), "甲ni\r\n乙");
    }

    #[test]
    fn index_shifts_tail_only() {
        let at = Composed::new(3, Some("ni"));
        assert_eq!(at.span(), 3..5);
        assert_eq!(at.index(0), 0);
        assert_eq!(at.index(3), 3);
        assert_eq!(at.index(4), 6);
        assert_eq!(at.caret(3), 5);
        assert_eq!(at.caret(4), 6);
    }
}
