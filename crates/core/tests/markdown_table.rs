//! 表格断块与单元格区间：其他块级构造起始行终止表格，单元格首尾空白按行级同一口径裁剪。

use atelyx_core::markdown::{parse_markdown, MarkdownBlock, ParseOptions};

fn blocks(source: &str) -> Vec<MarkdownBlock> {
    parse_markdown(source, &ParseOptions::default()).blocks
}

/// 表头 + 分隔行后接一段文本：返回表格行数与紧随其后的首块。
fn table_after(tail: &str) -> (usize, Option<MarkdownBlock>) {
    let source = format!("| a | b |\n| --- | --- |\n{tail}");
    let mut parsed = blocks(&source);
    let rest = (parsed.len() > 1).then(|| parsed.remove(1));
    let rows = match &parsed[0] {
        MarkdownBlock::Table { rows, .. } => rows.len(),
        other => panic!("首块应为表格：{other:?}"),
    };
    (rows, rest)
}

fn kind_of(block: &MarkdownBlock) -> &'static str {
    match block {
        MarkdownBlock::Heading { .. } => "heading",
        MarkdownBlock::FencedCode { .. } => "fencedCode",
        MarkdownBlock::IndentedCode { .. } => "indentedCode",
        MarkdownBlock::HtmlBlock { .. } => "htmlBlock",
        MarkdownBlock::Blockquote { .. } => "blockquote",
        MarkdownBlock::List { .. } => "list",
        MarkdownBlock::Hr { .. } => "hr",
        MarkdownBlock::MathBlock { .. } => "mathBlock",
        other => panic!("未预期块：{other:?}"),
    }
}

#[test]
fn block_start_line_ends_table() {
    let cases: &[(&str, &str)] = &[
        ("# 标题 | x\n", "heading"),
        ("```\ncode\n```\n", "fencedCode"),
        ("    code | x\n", "indentedCode"),
        ("<div>x</div>\n", "htmlBlock"),
        ("> 引用 | x\n", "blockquote"),
        ("- 列表 | x\n", "list"),
        ("---\n", "hr"),
        ("$$\nx | y\n$$\n", "mathBlock"),
    ];
    for (tail, want) in cases {
        let (rows, rest) = table_after(tail);
        assert_eq!(rows, 0, "{tail:?} 应终止表格");
        let rest = rest.unwrap_or_else(|| panic!("{tail:?} 后应有块"));
        assert_eq!(kind_of(&rest), *want, "{tail:?}");
    }
}

#[test]
fn plain_line_stays_single_cell_row() {
    let source = "| a | b |\n| --- | --- |\nplain text\n";
    let parsed = blocks(source);
    let MarkdownBlock::Table { rows, to, .. } = &parsed[0] else { panic!("首块应为表格") };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    assert_eq!(&source[rows[0][0].from..rows[0][0].to], "plain text");
    assert_eq!(*to, source.len() - 1);
    assert_eq!(parsed.len(), 1);
}

#[test]
fn cell_ranges_skip_all_whitespace() {
    // 全角空格与裸 `\r` 作单元格首尾空白时不计入区间
    let source = "| 　甲　 | \r b |\n| --- | --- |\n";
    let parsed = blocks(source);
    let MarkdownBlock::Table { header, .. } = &parsed[0] else { panic!("首块应为表格") };
    assert_eq!(&source[header[0].from..header[0].to], "甲");
    assert_eq!(&source[header[1].from..header[1].to], "b");
}

#[test]
fn whitespace_only_cell_is_empty() {
    let source = "| a | 　 |\n| --- | --- |\n";
    let parsed = blocks(source);
    let MarkdownBlock::Table { header, .. } = &parsed[0] else { panic!("首块应为表格") };
    assert_eq!(header[1].from, header[1].to);
}

#[test]
fn escaped_pipe_cell_is_not_whitespace() {
    let source = "| a | \\| |\n| --- | --- |\n";
    let parsed = blocks(source);
    let MarkdownBlock::Table { header, .. } = &parsed[0] else { panic!("首块应为表格") };
    assert_eq!(&source[header[1].from..header[1].to], "\\|");
}

#[test]
fn crlf_rows_trim_cell_edges() {
    let source = "| a | b |\r\n| --- | --- |\r\n| c | d |\r\n";
    let parsed = blocks(source);
    let MarkdownBlock::Table { header, rows, .. } = &parsed[0] else { panic!("首块应为表格") };
    let texts: Vec<&str> = header
        .iter()
        .chain(rows.iter().flatten())
        .map(|c| &source[c.from..c.to])
        .collect();
    assert_eq!(texts, ["a", "b", "c", "d"]);
}
