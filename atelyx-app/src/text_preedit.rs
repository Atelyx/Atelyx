//! 输入法组合串的排版口径：组合串作为光标处的临时字符进入段落，其后文本随排版推开。

use std::borrow::Cow;
use std::ops::Range;

/// 组合串的位置：插入点（正文字符下标）与串长（字符数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composed {
    insert: usize,
    len: usize,
}

impl Composed {
    /// `caret` 为插入点（正文字符下标）；空串视同未组合。
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
pub(crate) fn compose<'a>(text: &'a str, at: Composed, preedit: Option<&str>) -> Cow<'a, str> {
    let Some(pre) = preedit.filter(|p| !p.is_empty()) else {
        return Cow::Borrowed(text);
    };
    let split = text.char_indices().nth(at.insert).map_or(text.len(), |(b, _)| b);
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
