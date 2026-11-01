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

/// 列表行标记解析结果。
struct ListMarker {
    /// 是否任务项。
    task: bool,
    /// 任务框内勾选位的行内偏移（非任务项为 0）。
    task_char: usize,
}

/// 解析列表行首标记：缩进 + 标记 + 标记后一以上空白或行尾 + 可选任务框。
/// 标记后既无空白又非行尾（如 `-foo`）按非列表处理。
fn parse_list_marker(line: &str) -> Option<ListMarker> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'-' | b'+' | b'*') => i += 1,
        Some(c) if c.is_ascii_digit() => {
            while matches!(bytes.get(i), Some(c) if c.is_ascii_digit()) {
                i += 1;
            }
            if !matches!(bytes.get(i), Some(b'.' | b')')) {
                return None;
            }
            i += 1;
        }
        _ => return None,
    }
    let ws_start = i;
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    if i == ws_start && i < line.len() {
        return None;
    }
    let mut task = false;
    let mut task_char = 0;
    if bytes.get(i) == Some(&b'[')
        && matches!(bytes.get(i + 1), Some(b' ') | Some(b'x') | Some(b'X'))
        && bytes.get(i + 2) == Some(&b']')
    {
        task_char = i + 1;
        task = true;
    }
    Some(ListMarker { task, task_char })
}

/// 行尾风格归一：正文换行只认 `\n`，`\r\n` 与孤立 `\r` 都折成一个 `\n`。
/// 无 `\r` 时原样返回（粘贴常带 CRLF，见 is_cr）。
pub fn normalize_newlines(text: String) -> String {
    if !text.contains('\r') {
        return text;
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 任务框切换：`item_from` 所在列表项是任务项时翻转勾选态（`[ ]` ⇄ `[x]`）。
/// 返回 (新正文, 勾选位字节偏移)。
pub fn toggle_task(text: &str, item_from: usize) -> Option<(String, usize)> {
    let line_start = line_start_at(text, item_from);
    let line = &text[line_start..line_end_at(text, item_from)];
    let marker = parse_list_marker(line)?;
    if !marker.task {
        return None;
    }
    let at = line_start + marker.task_char;
    let ch = if text.as_bytes().get(at) == Some(&b' ') { "x" } else { " " };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..at]);
    out.push_str(ch);
    out.push_str(&text[at + 1..]);
    Some((out, at))
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

/// 编辑面状态（应用层持有）：光标/选区/活动块/撤销栈/组合态。
#[derive(Default)]
pub struct EditorState {
    pub caret: usize,
    pub anchor: usize,
    /// 当前源码态块序号（None = 无活动块，全篇按只读渲染）。
    pub active: Option<usize>,
    /// 输入法未上屏文本。
    pub preedit: Option<String>,
    /// 编辑面是否持有光标（点击编辑面内为真，点编辑面外为假）。
    pub focused: bool,
    undo: Vec<(String, usize, usize)>,
    redo: Vec<(String, usize, usize)>,
}

impl EditorState {
    /// 编辑前快照（用于撤销）。
    pub fn snapshot(&mut self, text: &str) {
        self.undo.push((text.to_string(), self.caret, self.anchor));
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// 有序选区 (from, to)。
    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }

    /// 撤销：返回 (正文, 光标, 锚点)。
    pub fn undo(&mut self, text: &str) -> Option<(String, usize, usize)> {
        let (prev, caret, anchor) = self.undo.pop()?;
        self.redo.push((text.to_string(), self.caret, self.anchor));
        Some((prev, caret, anchor))
    }

    /// 重做：返回 (正文, 光标, 锚点)。
    pub fn redo(&mut self, text: &str) -> Option<(String, usize, usize)> {
        let (next, caret, anchor) = self.redo.pop()?;
        self.undo.push((text.to_string(), self.caret, self.anchor));
        Some((next, caret, anchor))
    }

    /// 载入新文件后重置。
    pub fn reset(&mut self, caret: usize) {
        self.caret = caret;
        self.anchor = caret;
        self.active = None;
        self.preedit = None;
        self.focused = false;
        self.undo.clear();
        self.redo.clear();
    }
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
