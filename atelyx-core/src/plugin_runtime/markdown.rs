//! `query.markdown`：把 Markdown 文本按笔记同一套解析语义转成插件可排版的块。
//!
//! 只做解析与降级（块 / 行内片段 / 嵌套层数），不排版、不量算——宽度与字体的决定权在插件侧；
//! 嵌套块一律展开成平铺序列并用 `depth` 记层数，插件据此换算缩进与左侧标记。

use atelyx_plugin_protocol::method::{
    MarkdownBlock, MarkdownResult, MarkdownSpan,
};

use crate::markdown::{
    block_from, parse_inline, parse_markdown, InlineSpan, MarkdownBlock as CoreBlock, MarkdownListItem,
    ParseOptions,
};

/// 解析一份 Markdown 文本；链接形态不在此判定（插件只拿 `link` 目标做样式，不解析仓库路径）。
pub(crate) fn blocks(source: &str) -> MarkdownResult {
    let options = ParseOptions::default();
    let document = parse_markdown(source, &options);
    let mut out = Vec::new();
    flatten(&document.blocks, 0, &document.source, &options, &mut out);
    MarkdownResult { blocks: out }
}

fn flatten(
    blocks: &[CoreBlock],
    depth: u32,
    source: &str,
    options: &ParseOptions,
    out: &mut Vec<MarkdownBlock>,
) {
    for block in blocks {
        match block {
            CoreBlock::Heading { level, inline, .. } => out.push(MarkdownBlock::Heading {
                level: (*level).clamp(1, 6),
                depth,
                spans: spans(inline),
            }),
            CoreBlock::Paragraph { inline, .. } => out.push(MarkdownBlock::Paragraph {
                depth,
                spans: spans(inline),
            }),
            CoreBlock::Blockquote { callout, children, .. } => {
                let inner = depth + 1;
                for (index, child) in children.iter().enumerate() {
                    match child {
                        CoreBlock::Paragraph { inline, .. } => {
                            let mut block_spans = spans(inline);
                            // callout 标记并进首段：插件没有 callout 语义，读到这里仍能看见类型
                            if index == 0 {
                                if let Some(kind) = callout.as_deref().filter(|kind| !kind.is_empty()) {
                                    block_spans.insert(0, MarkdownSpan { text: format!("[!{kind}]"), ..MarkdownSpan::default() });
                                }
                            }
                            out.push(MarkdownBlock::Quote { depth: inner, spans: block_spans });
                        }
                        other => flatten(std::slice::from_ref(other), inner, source, options, out),
                    }
                }
            }
            CoreBlock::List { ordered, items, .. } => {
                for (index, item) in items.iter().enumerate() {
                    // 有序列表的序号取源标记（`3.` 就是 3）；无序项恒为 1
                    let ordinal = if *ordered {
                        item_ordinal(&source[item.from..item.to]).unwrap_or(index as u32 + 1)
                    } else {
                        1
                    };
                    out.push(MarkdownBlock::ListItem {
                        depth,
                        ordered: *ordered,
                        index: ordinal,
                        spans: item_spans(source, item, options),
                    });
                    flatten(&item.children, depth + 1, source, options, out);
                }
            }
            CoreBlock::FencedCode { lang, code, .. } => out.push(MarkdownBlock::Code {
                depth,
                lang: lang.clone(),
                content: code.clone(),
            }),
            CoreBlock::IndentedCode { code, .. } => out.push(MarkdownBlock::Code {
                depth,
                lang: String::new(),
                content: code.clone(),
            }),
            CoreBlock::MathBlock { tex, .. } => out.push(MarkdownBlock::Code {
                depth,
                lang: "math".into(),
                content: tex.clone(),
            }),
            CoreBlock::Hr { .. } => out.push(MarkdownBlock::Divider),
            CoreBlock::Table { header, rows, .. } => {
                let mut table: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
                table.push(header.iter().map(|cell| cell_text(source, cell.from, cell.to, options)).collect());
                for row in rows {
                    table.push(row.iter().map(|cell| cell_text(source, cell.from, cell.to, options)).collect());
                }
                out.push(MarkdownBlock::Table { depth, rows: table });
            }
            CoreBlock::HtmlBlock { html, .. } => out.push(MarkdownBlock::Paragraph {
                depth,
                spans: vec![MarkdownSpan { text: html.clone(), ..MarkdownSpan::default() }],
            }),
            CoreBlock::FootnoteDef { inline, label, .. } => {
                let mut spans = spans(inline);
                spans.insert(0, MarkdownSpan { text: format!("[{label}] "), ..MarkdownSpan::default() });
                out.push(MarkdownBlock::Paragraph { depth, spans });
            }
        }
    }
}

/// 列表项首段内容：解析产物只把项内后续块放进 `children`，首段正文留在源区间里按标记剥出。
/// 引用块内的项续行仍带 `>` 前缀，先逐行剥掉再按行内语义解析（与笔记渲染同口径）。
fn item_spans(source: &str, item: &MarkdownListItem, options: &ParseOptions) -> Vec<MarkdownSpan> {
    let content_from = item.from + item_marker_len(&source[item.from..item.to]);
    let content_end = item.children.first().map(block_from).unwrap_or(item.to).max(content_from);
    let content_to = content_from + source[content_from..content_end].trim_end().len();
    let content = strip_quote_marks(&source[content_from..content_to]);
    if content.trim().is_empty() {
        return Vec::new();
    }
    let mut out = spans(&parse_inline(&content, 0, content.len(), options));
    if item.task {
        let marker = if item.checked { "[x] " } else { "[ ] " };
        out.insert(0, MarkdownSpan { text: marker.into(), ..MarkdownSpan::default() });
    }
    out
}

/// 逐行剥掉引用前缀（缩进 ≤3 + `>` + 至多一个空格，可叠层）。
fn strip_quote_marks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let mut rest = line;
        while let Some(width) = quote_prefix_len(rest) {
            rest = &rest[width..];
        }
        out.push_str(rest);
    }
    out
}

fn quote_prefix_len(text: &str) -> Option<usize> {
    let indent = text.len() - text.trim_start_matches([' ', '\t']).len();
    if indent > 3 || !text[indent..].starts_with('>') {
        return None;
    }
    let after = &text[indent + 1..];
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    Some(indent + 1 + spaces.min(1))
}

/// 有序列表项在源里的序号（`3.` / `3)` → 3）；非有序标记返回 None。
fn item_ordinal(raw: &str) -> Option<u32> {
    let start = raw.len() - raw.trim_start_matches([' ', '\t']).len();
    let rest = &raw[start..];
    let digits = rest.find(['.', ')'])?;
    if digits == 0 {
        return None;
    }
    rest[..digits].parse::<u32>().ok()
}

/// 列表标记（含任务框）的字节长度：与解析器产出的项区间口径一致（`-` / `*` / `+` / `1.` / `1)`）。
fn item_marker_len(raw: &str) -> usize {
    let bytes = raw.as_bytes();
    let skip_ws = |mut i: usize| {
        while matches!(bytes.get(i), Some(b' ') | Some(b'\t')) {
            i += 1;
        }
        i
    };
    let mut i = skip_ws(0);
    match bytes.get(i) {
        Some(b'-') | Some(b'+') | Some(b'*') => i += 1,
        Some(c) if c.is_ascii_digit() => {
            while matches!(bytes.get(i), Some(c) if c.is_ascii_digit()) {
                i += 1;
            }
            if !matches!(bytes.get(i), Some(b'.') | Some(b')')) {
                return 0;
            }
            i += 1;
        }
        _ => return 0,
    }
    i = skip_ws(i);
    if bytes.get(i) == Some(&b'[')
        && matches!(bytes.get(i + 1), Some(b' ') | Some(b'x') | Some(b'X'))
        && bytes.get(i + 2) == Some(&b']')
    {
        i = skip_ws(i + 3);
    }
    i
}

/// 表格单元格只给纯文本：按行内语义解析后拼成文字（`**` / `` ` `` / `\|` 等标记不进入正文）。
fn cell_text(source: &str, from: usize, to: usize, options: &ParseOptions) -> String {
    if from >= to {
        return String::new();
    }
    let inline = parse_inline(source, from, to, options);
    spans(&inline)
        .into_iter()
        .map(|span| span.text)
        .collect::<String>()
        .trim()
        .to_string()
}

/// 行内片段拍平：样式只保留字重（`strong`）、等宽（`code`）与链接目标，其余降级为正文。
fn spans(inline: &[InlineSpan]) -> Vec<MarkdownSpan> {
    let mut out = Vec::new();
    push_spans(inline, false, &mut out);
    out
}

fn push_spans(inline: &[InlineSpan], strong: bool, out: &mut Vec<MarkdownSpan>) {
    for span in inline {
        match span {
            InlineSpan::Text { text, .. } => push_text(out, text, strong, false, None),
            InlineSpan::Code { text, .. } => push_text(out, text, strong, true, None),
            InlineSpan::MathInline { tex, .. } => push_text(out, tex, strong, true, None),
            InlineSpan::Strong { children, .. } | InlineSpan::Highlight { children, .. } => {
                push_spans(children, true, out);
            }
            InlineSpan::Em { children, .. } | InlineSpan::Strike { children, .. } => {
                push_spans(children, strong, out);
            }
            InlineSpan::Link { label, href, .. } => push_text(out, label, strong, false, Some(href.clone())),
            InlineSpan::Autolink { label, href, .. } => push_text(out, label, strong, false, Some(href.clone())),
            InlineSpan::Wiki { label, target, .. } => push_text(out, label, strong, false, Some(target.clone())),
            InlineSpan::Image { alt, .. } => push_text(out, alt, strong, false, None),
            InlineSpan::Tag { tag, .. } => push_text(out, &format!("#{tag}"), strong, false, None),
            InlineSpan::FootnoteRef { label, .. } => push_text(out, &format!("[{label}]"), strong, false, None),
            InlineSpan::Mention { label, .. } => push_text(out, label, strong, false, None),
            InlineSpan::Html { html, .. } => push_text(out, html, strong, false, None),
            InlineSpan::HardBreak { .. } => push_text(out, "\n", false, false, None),
            InlineSpan::Comment { .. } => {}
        }
    }
}

fn push_text(out: &mut Vec<MarkdownSpan>, text: &str, strong: bool, code: bool, link: Option<String>) {
    if text.is_empty() {
        return;
    }
    out.push(MarkdownSpan { text: text.to_string(), strong, code, link });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(blocks: &[MarkdownBlock]) -> Vec<String> {
        blocks
            .iter()
            .map(|block| match block {
                MarkdownBlock::Heading { level, depth, spans } => {
                    format!("h{level}/{depth}:{}", plain(spans))
                }
                MarkdownBlock::Paragraph { depth, spans } => format!("p{depth}:{}", plain(spans)),
                MarkdownBlock::ListItem { depth, ordered, index, spans } => {
                    format!("li{depth}/{ordered}/{index}:{}", plain(spans))
                }
                MarkdownBlock::Quote { depth, spans } => format!("q{depth}:{}", plain(spans)),
                MarkdownBlock::Code { depth, lang, content } => format!("code{depth}/{lang}:{content}"),
                MarkdownBlock::Table { depth, rows } => format!("table{depth}:{rows:?}"),
                MarkdownBlock::Divider => "hr".into(),
            })
            .collect()
    }

    fn plain(spans: &[MarkdownSpan]) -> String {
        spans.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn headings_paragraphs_and_inline_styles_survive() {
        let result = blocks("# 标题\n\n正文 **加粗** 与 `代码` 和 [链接](https://example.com)。");
        let blocks = summary(&result.blocks);
        assert_eq!(blocks[0], "h1/0:标题");
        assert_eq!(blocks[1], "p0:正文 加粗 与 代码 和 链接。");
        let paragraph = match &result.blocks[1] {
            MarkdownBlock::Paragraph { spans, .. } => spans,
            _ => unreachable!(),
        };
        assert!(paragraph.iter().any(|span| span.text == "加粗" && span.strong));
        assert!(paragraph.iter().any(|span| span.text == "代码" && span.code));
        assert!(paragraph
            .iter()
            .any(|span| span.text == "链接" && span.link.as_deref() == Some("https://example.com")));
    }

    #[test]
    fn nested_lists_flatten_with_depth_and_keep_their_index() {
        let result = blocks("1. 一\n2. 二\n   - 子项\n");
        let blocks = summary(&result.blocks);
        assert_eq!(blocks[0], "li0/true/1:一");
        assert_eq!(blocks[1], "li0/true/2:二");
        assert_eq!(blocks[2], "li1/false/1:子项");
    }

    #[test]
    fn quote_paragraphs_carry_depth_and_callout_marker() {
        let result = blocks("> [!note] 提示\n> 引用正文\n");
        let blocks = summary(&result.blocks);
        assert_eq!(blocks[0], "q1:[!note] 提示\n引用正文", "软换行留在片段里，由插件排版成换行");
    }

    #[test]
    fn code_blocks_tables_and_dividers_map_to_their_blocks() {
        let result = blocks("```rust\nlet a = 1;\n```\n\n| 甲 | 乙 |\n| --- | --- |\n| 1 | 2 |\n\n---\n");
        let blocks = summary(&result.blocks);
        assert_eq!(blocks[0], "code0/rust:let a = 1;");
        assert!(blocks[1].starts_with("table0:"), "{}", blocks[1]);
        assert!(blocks[1].contains("甲") && blocks[1].contains("2"));
        assert_eq!(blocks[2], "hr");
    }

    #[test]
    fn task_items_get_a_checkbox_prefix() {
        let result = blocks("- [x] 已完成\n- [ ] 未完成\n");
        let blocks = summary(&result.blocks);
        assert_eq!(blocks[0], "li0/false/1:[x] 已完成");
        assert_eq!(blocks[1], "li0/false/1:[ ] 未完成", "无序项序号恒为 1");
    }

    #[test]
    fn quote_nested_list_items_drop_the_prefix() {
        let result = blocks("> - 甲\n>   续行\n");
        let blocks = summary(&result.blocks);
        assert_eq!(blocks[0], "li1/false/1:甲\n  续行", "项续行的引用标记不进正文，缩进保留");
    }

    #[test]
    fn ordered_items_keep_their_source_ordinal() {
        let result = blocks("3. 三\n4. 四\n");
        let blocks = summary(&result.blocks);
        assert_eq!(blocks[0], "li0/true/3:三");
        assert_eq!(blocks[1], "li0/true/4:四");
    }

    #[test]
    fn table_cells_decode_escapes_and_inline_markup() {
        let result = blocks("| **甲** | 乙 \\| 丙 |\n| --- | --- |\n| `1` | 2 |\n");
        let rows = match &result.blocks[0] {
            MarkdownBlock::Table { rows, .. } => rows.clone(),
            other => panic!("应为表格块：{other:?}"),
        };
        assert_eq!(rows[0], vec!["甲", "乙 | 丙"], "转义竖线与强调标记不进入正文");
        assert_eq!(rows[1], vec!["1", "2"]);
    }
}
