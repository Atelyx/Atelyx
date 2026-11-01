//! 块内文本编辑的纯变换：列表续项 / 空项退格 / 任务勾选 / 缩进与反缩进。
//! 只处理源文与选区，不碰界面状态。

/// 一级缩进单位（列表靠行首缩进判定嵌套）。
pub const INDENT_UNIT: &str = "  ";

/// 文本变换结果：新文本 + 变换后的光标（源偏移）。
pub struct LineEdit {
    pub text: String,
    pub caret: usize,
}

/// `offset` 所在行的行首偏移（非字符边界时向下取整）。
pub fn line_start_at(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[..offset].rfind('\n').map_or(0, |i| i + 1)
}

/// 自 `offset` 起本行行尾偏移（不含换行符）。
pub fn line_end_at(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[offset..].find('\n').map_or(text.len(), |i| offset + i)
}

fn line_text_at(text: &str, offset: usize) -> &str {
    let start = line_start_at(text, offset);
    &text[start..line_end_at(text, start)]
}

/// 列表行标记解析结果。
struct ListMarker {
    /// 行首缩进字节长度。
    indent: usize,
    /// 标记整体末尾（缩进 + 标记 + 标记后空白 + 任务框）。
    end: usize,
    /// 有序项序号（无序为 None）。
    ordinal: Option<u64>,
    /// 有序定界符 `'.'` / `')'`；无序为 0。
    delim: u8,
    /// 无序标记字符；有序为 0。
    bullet: u8,
    /// 是否任务项。
    task: bool,
}

/// 解析列表行首标记：缩进 + 标记 + 标记后一以上空白或行尾 + 可选任务框。
/// 标记后既无空白又非行尾（如 `-foo`）按非列表处理。
fn parse_list_marker(line: &str) -> Option<ListMarker> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    let indent = i;
    let (bullet, ordinal, delim) = match bytes.get(i) {
        Some(c @ (b'-' | b'+' | b'*')) => {
            i += 1;
            (*c, None, 0)
        }
        Some(c) if c.is_ascii_digit() => {
            let start = i;
            while matches!(bytes.get(i), Some(c) if c.is_ascii_digit()) {
                i += 1;
            }
            let delim = match bytes.get(i) {
                Some(d @ (b'.' | b')')) => *d,
                _ => return None,
            };
            let ordinal = line[start..i].parse().ok()?;
            i += 1;
            (0, Some(ordinal), delim)
        }
        _ => return None,
    };
    let ws_start = i;
    while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
        i += 1;
    }
    if i == ws_start && i < line.len() {
        return None;
    }
    let mut task = false;
    if bytes.get(i) == Some(&b'[')
        && matches!(bytes.get(i + 1), Some(b' ') | Some(b'x') | Some(b'X'))
        && bytes.get(i + 2) == Some(&b']')
    {
        i += 3;
        while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
            i += 1;
        }
        task = true;
    }
    Some(ListMarker { indent, end: i, ordinal, delim, bullet, task })
}

fn is_list_line(line: &str) -> bool {
    parse_list_marker(line).is_some()
}

/// 选区覆盖的行首偏移：折叠光标即当前行；有选区时取最后一个被选中字符所在行。
fn covered_line_starts(text: &str, from: usize, to: usize) -> Vec<usize> {
    let last = line_start_at(text, if to > from { to - 1 } else { from });
    let mut starts = Vec::new();
    let mut start = line_start_at(text, from);
    loop {
        starts.push(start);
        if start >= last {
            return starts;
        }
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return starts,
        }
    }
}

/// 行尾风格归一：正文换行只认 `\n`，`\r\n` 与孤立 `\r` 都折成一个 `\n`。
pub fn normalize_newlines(text: String) -> String {
    if !text.contains('\r') {
        return text;
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 列表行 Enter 的文本变换：延续当前项——无序沿用同字符、有序序号 +1、任务项带空任务框。
/// 光标停在标记之前时返回 None（那是「在标记前换行」，续标记会拆出嵌套列表）。
pub fn list_enter_edit(text: &str, from: usize) -> Option<(String, usize)> {
    let line_start = line_start_at(text, from);
    let line = &text[line_start..line_end_at(text, from)];
    let marker = parse_list_marker(line)?;
    if from < line_start + marker.end {
        return None;
    }
    let indent = &text[line_start..line_start + marker.indent];
    let next = match marker.ordinal {
        Some(n) => format!("{}{}", n + 1, marker.delim as char),
        None => (marker.bullet as char).to_string(),
    };
    let insert = format!("\n{indent}{next} {}", if marker.task { "[ ] " } else { "" });
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..from]);
    out.push_str(&insert);
    out.push_str(&text[from..]);
    Some((out, from + insert.len()))
}

/// 空列表项行上的退格变换：行内只有标记且光标落在标记之内或之后时，一次删掉整段标记。
pub fn list_backspace_edit(text: &str, from: usize) -> Option<(String, usize)> {
    let line_start = line_start_at(text, from);
    let line = &text[line_start..line_end_at(text, from)];
    let marker = parse_list_marker(line)?;
    if marker.end != line.len() {
        return None;
    }
    let marker_end = line_start + marker.end;
    if from <= line_start || from > marker_end {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..line_start]);
    out.push_str(&text[marker_end..]);
    Some((out, line_start))
}

/// Tab 缩进：光标在列表行或存在选区时整行缩进，其余情况在光标处插入一级缩进。
pub fn indent_edit(text: &str, from: usize, to: usize) -> LineEdit {
    if from == to && !is_list_line(line_text_at(text, from)) {
        let mut out = String::with_capacity(text.len() + INDENT_UNIT.len());
        out.push_str(&text[..from]);
        out.push_str(INDENT_UNIT);
        out.push_str(&text[from..]);
        return LineEdit { text: out, caret: from + INDENT_UNIT.len() };
    }
    let starts = covered_line_starts(text, from, to);
    let shift = |offset: usize| {
        offset + INDENT_UNIT.len() * starts.iter().filter(|s| **s <= offset).count()
    };
    let mut out = String::with_capacity(text.len() + INDENT_UNIT.len() * starts.len());
    let mut prev = 0;
    for &start in &starts {
        out.push_str(&text[prev..start]);
        out.push_str(INDENT_UNIT);
        prev = start;
    }
    out.push_str(&text[prev..]);
    LineEdit { text: out, caret: shift(from) }
}

/// Shift+Tab 反缩进：选区覆盖的每一行去掉一级前导缩进；没有可删的行返回 None。
pub fn outdent_edit(text: &str, from: usize, to: usize) -> Option<LineEdit> {
    let mut removals: Vec<(usize, usize)> = Vec::new();
    for start in covered_line_starts(text, from, to) {
        let rest = &text[start..];
        let count = if rest.starts_with('\t') {
            1
        } else {
            let mut n = 0;
            while n < INDENT_UNIT.len() && rest.as_bytes().get(n) == Some(&b' ') {
                n += 1;
            }
            n
        };
        if count > 0 {
            removals.push((start, count));
        }
    }
    if removals.is_empty() {
        return None;
    }
    let map = |offset: usize| -> usize {
        let mut removed = 0;
        for &(start, count) in &removals {
            if start >= offset {
                break;
            }
            if offset < start + count {
                return start - removed;
            }
            removed += count;
        }
        offset - removed
    };
    let mut out = String::with_capacity(text.len());
    let mut prev = 0;
    for &(start, count) in &removals {
        out.push_str(&text[prev..start]);
        prev = start + count;
    }
    out.push_str(&text[prev..]);
    Some(LineEdit { text: out, caret: map(from) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_continues_unordered_and_ordered_items() {
        let (text, caret) = list_enter_edit("- 甲\n", 5).expect("列表行");
        assert_eq!(text, "- 甲\n- \n");
        assert_eq!(caret, 8);
        let (text, _) = list_enter_edit("2) 乙\n", 6).expect("有序行");
        assert_eq!(text, "2) 乙\n3) \n");
        assert!(list_enter_edit("- 甲\n", 0).is_none(), "标记前回车不续项");
    }

    #[test]
    fn backspace_on_empty_item_removes_the_marker() {
        let (text, caret) = list_backspace_edit("- \n", 2).expect("空项");
        assert_eq!(text, "\n");
        assert_eq!(caret, 0);
        assert!(list_backspace_edit("- 甲\n", 3).is_none(), "有内容不改写");
    }

    #[test]
    fn indent_and_outdent_keep_the_caret() {
        let edit = indent_edit("甲\n乙\n", 0, 8);
        assert_eq!(edit.text, "  甲\n  乙\n");
        assert_eq!(edit.caret, 2);
        let back = outdent_edit(&edit.text, 2, 12).expect("可反缩进");
        assert_eq!(back.text, "甲\n乙\n");
        assert_eq!(back.caret, 0);
    }
}
