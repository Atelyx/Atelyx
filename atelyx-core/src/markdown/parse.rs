//! 块级解析：行驱动状态机，块规格语义由对拍快照 `fixtures/markdown/parse.json` 锁定。
//!
//! 容器块（引用/列表项）递归采用「等宽虚拟缓冲」：前缀区间替换为等宽空格后
//! 全量重解析，块区间平移回原偏移——保证子块区间与快照逐值一致。
//! 所有偏移均为字节偏移（对拍 harness 负责 UTF-16 ↔ 字节转换）。

use super::inline;
use super::types::*;
use std::collections::HashSet;

/// 数学消费状态：预扫区间顶层共享，跨段落区间只消费一次。
struct MathState {
    ranges: Vec<RangeInfo>,
    used: HashSet<usize>,
}

pub(super) struct Ctx<'a> {
    pub source: &'a str,
    pub options: &'a ParseOptions,
    /// LinkReference/脚注定义段落：URL 部分不参与裸 URL autolink。
    pub no_bare_autolink: bool,
}

pub fn parse_markdown(source: &str, options: &ParseOptions) -> MarkdownDocument {
    let ctx = Ctx { source, options, no_bare_autolink: false };
    // 第一遍取盲区：fence/缩进代码/HTML 块内的 `$$` 不是数学定界符
    let blind: Vec<RangeInfo> = parse_blocks(&ctx, 0, source.len(), None)
        .into_iter()
        .filter(|b| {
            matches!(
                b,
                MarkdownBlock::FencedCode { .. }
                    | MarkdownBlock::IndentedCode { .. }
                    | MarkdownBlock::HtmlBlock { .. }
            )
        })
        .map(|b| RangeInfo { from: block_from(&b), to: block_to(&b) })
        .collect();
    let ranges: Vec<RangeInfo> = block_math_ranges(source)
        .into_iter()
        .filter(|r| !blind.iter().any(|o| r.from < o.to && r.to > o.from))
        .collect();
    let blocks = parse_blocks(
        &ctx,
        0,
        source.len(),
        Some(&mut MathState { ranges, used: HashSet::new() }),
    );
    MarkdownDocument { source: source.to_string(), blocks }
}

/// 按源区间做行内解析：表格单元格与列表项内容在块规格里只带 from/to，
/// 行内片段由渲染层按区间重解析（与正文同一套行内语义）。
pub fn parse_inline(source: &str, from: usize, to: usize, options: &ParseOptions) -> Vec<InlineSpan> {
    let ctx = Ctx { source, options, no_bare_autolink: false };
    inline::parse_inline_range(&ctx, from, to)
}

// ===== 行原语 =====

/// 行视图：行尾偏移在 `\n` 之前（CRLF 源文本的 `\r` 属行内容）。
#[derive(Clone, Copy)]
struct Line<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

fn read_line(src: &str, pos: usize, to: usize) -> Line<'_> {
    let nl = src[pos..to].find('\n').map(|i| pos + i).unwrap_or(to);
    Line { start: pos, end: nl, text: &src[pos..nl] }
}

/// 下一行起点（`\n` 之后；无 `\n` 则到块尾）。
fn next_pos(line: &Line, to: usize, src: &str) -> usize {
    match src[line.start..to].find('\n') {
        Some(i) => (line.start + i + 1).min(to),
        None => to,
    }
}

fn next_after(src: &str, end: usize, to: usize) -> usize {
    match src[end..to].find('\n') {
        Some(i) => end + i + 1,
        None => to,
    }
}

fn trim_indent(text: &str) -> &str {
    text.trim_start_matches([' ', '\t'])
}

/// 行首空白从 `col` 列起算的（字节数，列数）。Tab 前进到 4 的下一档列。
fn indent_from(text: &str, col: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut c = col;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b' ' => c += 1,
            b'\t' => c = (c / 4 + 1) * 4,
            _ => break,
        }
        i += 1;
    }
    (i, c - col)
}

fn indent_prefix(text: &str) -> (usize, usize) {
    indent_from(text, 0)
}

fn indent_prefix_len(text: &str) -> usize {
    indent_prefix(text).0
}

fn indent_columns(text: &str) -> usize {
    indent_prefix(text).1
}

/// 容器剥前缀：消耗 `want` 列后，行首剩余空白折成等宽空格，返回（跳过字节数，空格列数）。
/// Tab 整格消费（对齐到列档），剩余列须从消费后的列位置起算，故一律折成空格。
fn fold_indent(text: &str, want: usize) -> (usize, usize) {
    let (stop, overshoot) = consume_columns(text, want);
    let (rest_bytes, rest_cols) = indent_from(&text[stop..], want + overshoot);
    (stop + rest_bytes, rest_cols)
}

/// 从行首消耗至多 `want` 列缩进：返回（跳过字节数，Tab 越档额外消耗的列数）。
/// Tab 对齐到下一列档，所得字节位置即消耗后的起点。
fn consume_columns(text: &str, want: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut col = 0usize;
    let mut i = 0usize;
    while col < want && i < bytes.len() {
        match bytes[i] {
            b' ' => col += 1,
            b'\t' => {
                let next = (col / 4 + 1) * 4;
                if next > want {
                    return (i + 1, next - want);
                }
                col = next;
            }
            _ => break,
        }
        i += 1;
    }
    (i, 0)
}

fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

// ===== 主解析 =====

fn parse_blocks(
    ctx: &Ctx,
    from: usize,
    to: usize,
    mut math: Option<&mut MathState>,
) -> Vec<MarkdownBlock> {
    let src = ctx.source;
    let mut blocks = Vec::new();
    let mut pos = from;
    while pos < to {
        let line = read_line(src, pos, to);
        if is_blank(line.text) {
            pos = next_pos(&line, to, src);
            continue;
        }
        let (ind, ind_cols) = indent_prefix(line.text);
        let body = &line.text[ind..];

        if ind_cols <= 3 {
            if let Some(fence) = fence_open(body) {
                let (block, next) = collect_fenced_code(ctx, &line, ind, fence, to);
                blocks.push(block);
                pos = next;
                continue;
            }
            if let Some(h) = atx_heading(body) {
                // 块区间 = 整行（含尾 # 序列）；行内片段 = 剥标记后的内容区间
                // h.from/h.to 相对去掉行首缩进的 body，回加 ind 才是行内绝对偏移
                let base = line.start + ind;
                let inline = inline::parse_inline_range(ctx, base + h.from, base + h.to);
                blocks.push(MarkdownBlock::Heading {
                    level: h.level,
                    from: base,
                    to: line.end,
                    inline,
                });
                pos = next_pos(&line, to, src);
                continue;
            }
            if let Some(kind) = html_block_open(body) {
                let (block, next) = collect_html_block(ctx, &line, to, kind);
                blocks.push(block);
                pos = next;
                continue;
            }
            if body.starts_with('>') {
                let (block, next) = collect_blockquote(ctx, &line, to);
                blocks.push(block);
                pos = next;
                continue;
            }
            if let Some(kind) = list_kind(body) {
                let (block, next) = collect_list(ctx, &line, ind, ind_cols, kind, to);
                blocks.push(block);
                pos = next;
                continue;
            }
            if is_hr(body) {
                blocks.push(MarkdownBlock::Hr { from: line.start + ind, to: line.end });
                pos = next_pos(&line, to, src);
                continue;
            }
            if body.contains('|') {
                if let Some(next_line) = peek_next_line(src, &line, to) {
                    if is_table_delim_row(trim_indent(next_line.text)) {
                        let (block, next) = collect_table(ctx, &line, to);
                        blocks.push(block);
                        pos = next;
                        continue;
                    }
                }
            }
        }
        if ind_cols >= 4 {
            let (block, next) = collect_indented_code(ctx, &line, to);
            blocks.push(block);
            pos = next;
            continue;
        }
        let (outs, next) = collect_paragraph(ctx, &line, to, math.as_deref_mut());
        blocks.extend(outs);
        pos = next;
    }
    blocks
}

fn peek_next_line<'a>(src: &'a str, line: &Line, to: usize) -> Option<Line<'a>> {
    let next = next_pos(line, to, src);
    (next < to).then(|| read_line(src, next, to))
}

// ===== 围栏代码 =====

struct FenceOpen {
    ch: u8,
    run: usize,
}

fn fence_open(body: &str) -> Option<FenceOpen> {
    let b = body.as_bytes();
    let ch = *b.first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let run = run_len(b, 0, b.len(), ch);
    if run < 3 {
        return None;
    }
    // 反引号围栏的 info 串里不得再有反引号
    if ch == b'`' && body[run..].contains('`') {
        return None;
    }
    Some(FenceOpen { ch, run })
}

fn collect_fenced_code(ctx: &Ctx, first: &Line, ind: usize, fence: FenceOpen, to: usize) -> (MarkdownBlock, usize) {
    let src = ctx.source;
    let lang = trim_indent(&first.text[ind + fence.run..]).trim().to_string();
    let open_line_end = first.end;
    let mut pos = next_pos(first, to, src);
    let mut close_start = to;
    let mut last_content_end = open_line_end; // 空正文时内容为点区间
    while pos < to {
        let line = read_line(src, pos, to);
        let body = trim_indent(line.text);
        let closes = {
            let run = run_len(body.as_bytes(), 0, body.len(), fence.ch);
            indent_columns(line.text) <= 3 && run >= fence.run && body[run..].trim().is_empty()
        };
        if closes {
            close_start = line.start;
            break;
        }
        if !is_blank(line.text) {
            last_content_end = line.end;
        }
        pos = next_pos(&line, to, src);
    }
    let content_from = (open_line_end + 1).min(close_start);
    let content_to = if last_content_end > content_from {
        last_content_end.min(close_start)
    } else {
        content_from
    };
    // 闭围栏行行尾即块尾（含空行正文时的 fallback 语义）
    let close_line_end = if close_start < to { read_line(src, close_start, to).end } else { to };
    let block = MarkdownBlock::FencedCode {
        from: first.start + ind,
        to: close_line_end,
        lang,
        code: src[content_from..content_to].to_string(),
        content_from,
        content_to,
    };
    let next = if close_start < to {
        next_pos(&read_line(src, close_start, to), to, src)
    } else {
        to
    };
    (block, next)
}

fn run_len(bytes: &[u8], pos: usize, to: usize, ch: u8) -> usize {
    let mut n = 0;
    while pos + n < to && bytes[pos + n] == ch {
        n += 1;
    }
    n
}

// ===== ATX 标题 =====

struct AtxContent {
    level: u8,
    /// 内容区间（body 内偏移）。
    from: usize,
    to: usize,
}

fn atx_heading(body: &str) -> Option<AtxContent> {
    let b = body.as_bytes();
    let run = run_len(b, 0, b.len(), b'#');
    if run == 0 || run > 6 {
        return None;
    }
    // `#` 后必须是空白或行尾
    if !b.get(run).is_none_or(|&c| c == b' ' || c == b'\t') {
        return None;
    }
    let content_from = run + b[run..].iter().take_while(|&&c| c == b' ' || c == b'\t').count();
    let mut content_to = body.trim_end().len();
    // 尾随 `#` 序列：前置须为空白（或整行只剩 `#`）
    let tail_start = body[..content_to].trim_end_matches('#').len();
    if tail_start < content_to {
        let before = body[..tail_start].trim_end();
        if before.len() < tail_start || before.is_empty() {
            content_to = before.len();
        }
    }
    Some(AtxContent { level: run as u8, from: content_from, to: content_to })
}

// ===== HTML 块 =====

#[derive(Clone, Copy, PartialEq)]
enum HtmlKind {
    /// `<script`/`<pre`/`<style`/`<textarea`：至对应闭标签。
    Raw(&'static str),
    Comment,
    Processing,
    Declaration,
    Cdata,
    /// type6：块级标签名，空行止。
    Named,
    /// type7：完整标签独行，空行止。
    Complete,
}

fn html_block_open(body: &str) -> Option<HtmlKind> {
    let lower = body.to_ascii_lowercase();
    for (open, close) in [
        ("<script", "</script>"),
        ("<pre", "</pre>"),
        ("<style", "</style>"),
        ("<textarea", "</textarea>"),
    ] {
        if lower.starts_with(open) {
            let after = body.as_bytes().get(open.len());
            if matches!(after, None | Some(b' ') | Some(b'\t') | Some(b'>')) {
                return Some(HtmlKind::Raw(close));
            }
        }
    }
    if body.starts_with("<!--") {
        return Some(HtmlKind::Comment);
    }
    if body.starts_with("<?") {
        return Some(HtmlKind::Processing);
    }
    if body.starts_with("<![CDATA[") {
        return Some(HtmlKind::Cdata);
    }
    if body.starts_with("<!") && body.as_bytes().get(2).is_some_and(u8::is_ascii_uppercase) {
        return Some(HtmlKind::Declaration);
    }
    let inner = body.strip_prefix("</").unwrap_or(body.strip_prefix('<')?);
    let name_len = inner
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .count();
    let name = &inner[..name_len];
    if BLOCK_TAGS.contains(&name.to_ascii_lowercase().as_str()) {
        let after = inner.as_bytes().get(name_len);
        if matches!(after, None | Some(b' ') | Some(b'\t') | Some(b'>') | Some(b'/')) {
            return Some(HtmlKind::Named);
        }
    }
    // type7：完整标签且行内无其他文本
    if let Some(gt) = body.find('>') {
        let inside = &body[1..gt];
        let valid = if let Some(name) = inside.strip_prefix('/') {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ' ' || c == '\t')
        } else {
            is_valid_open_tag(inside)
        };
        if valid && body[gt + 1..].trim().is_empty() {
            return Some(HtmlKind::Complete);
        }
    }
    None
}

/// 开标签宽松校验：标签名 + 属性字符不含 `<`/`>`（引号内除外）。
fn is_valid_open_tag(s: &str) -> bool {
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let mut in_quote: Option<char> = None;
    for c in s.chars() {
        match in_quote {
            Some(q) if c == q => in_quote = None,
            Some(_) => {}
            None => {
                if c == '"' || c == '\'' {
                    in_quote = Some(c);
                } else if c == '<' || c == '>' {
                    return false;
                }
            }
        }
    }
    true
}

const BLOCK_TAGS: &[&str] = &[
    "address", "article", "aside", "base", "basefont", "blockquote", "body", "caption", "center",
    "col", "colgroup", "dd", "details", "dialog", "dir", "div", "dl", "dt", "fieldset",
    "figcaption", "figure", "footer", "form", "frame", "frameset", "h1", "h2", "h3", "h4", "h5",
    "h6", "head", "header", "hr", "html", "iframe", "legend", "li", "link", "main", "menu",
    "menuitem", "nav", "noframes", "ol", "optgroup", "option", "p", "param", "section", "source",
    "summary", "table", "tbody", "td", "tfoot", "th", "thead", "title", "tr", "track", "ul",
];

fn collect_html_block(ctx: &Ctx, first: &Line, to: usize, kind: HtmlKind) -> (MarkdownBlock, usize) {
    let src = ctx.source;
    let from = first.start + indent_prefix_len(first.text);
    let mut pos = next_pos(first, to, src);
    let mut last = first.end;
    let done = |line: &Line| -> bool {
        match kind {
            HtmlKind::Raw(close) => contains_ignore_case(line.text, close.as_bytes()),
            HtmlKind::Comment => line.text.contains("-->"),
            HtmlKind::Processing => line.text.contains("?>"),
            HtmlKind::Declaration => line.text.contains('>'),
            HtmlKind::Cdata => line.text.contains("]]>"),
            HtmlKind::Named | HtmlKind::Complete => false,
        }
    };
    match kind {
        HtmlKind::Named | HtmlKind::Complete => {
            while pos < to {
                let line = read_line(src, pos, to);
                if is_blank(line.text) {
                    break;
                }
                last = line.end;
                pos = next_pos(&line, to, src);
            }
        }
        _ => {
            // 终止行（含结束标记的行，可为首行）计入块尾
            if !done(first) {
                while pos < to {
                    let line = read_line(src, pos, to);
                    last = line.end;
                    pos = next_pos(&line, to, src);
                    if done(&line) {
                        break;
                    }
                }
            }
        }
    }
    let block = MarkdownBlock::HtmlBlock {
        from,
        to: last,
        html: src[from..last].to_string(),
    };
    (block, next_after(src, last, to))
}

fn contains_ignore_case(haystack: &str, needle: &[u8]) -> bool {
    let h = haystack.as_bytes();
    if needle.is_empty() || h.len() < needle.len() {
        return false;
    }
    h.windows(needle.len())
        .any(|w| w.iter().zip(needle).all(|(a, b)| a.eq_ignore_ascii_case(b)))
}

// ===== 引用块 =====

/// 一层引用前缀 `[ ]{0,3}>` + 至多 1 列空白：返回（跳过字节数，剩余缩进折成的空格列数）。
fn quote_prefix(text: &str) -> Option<(usize, usize)> {
    let (ind, ind_cols) = indent_prefix(text);
    if ind_cols > 3 || !text[ind..].starts_with('>') {
        return None;
    }
    let (ws_bytes, ws_cols) = indent_from(&text[ind + 1..], ind_cols + 1);
    Some((ind + 1 + ws_bytes, ws_cols.saturating_sub(1)))
}

/// 段落续行判定（lazy continuation 依据）：非空、缩进 <4、不是任何块起始。
fn is_paragraph_continuation(text: &str) -> bool {
    if is_blank(text) {
        return false;
    }
    let (ind, ind_cols) = indent_prefix(text);
    if ind_cols >= 4 {
        return false;
    }
    let body = &text[ind..];
    if body.starts_with('>')
        || fence_open(body).is_some()
        || atx_heading(body).is_some()
        || is_hr(body)
        || list_kind(body).is_some_and(|k| k.has_content())
        || html_block_open(body).is_some()
        || is_link_ref_def_line(body)
    {
        return false;
    }
    true
}
/// 链接/脚注定义行形态（独立成块并打断所在段落）。
fn is_link_ref_def_line(body: &str) -> bool {
    let Some(rest) = body.strip_prefix('[') else {
        return false;
    };
    let Some(rb) = rest.find(']') else {
        return false;
    };
    match rest[rb + 1..].strip_prefix(':') {
        Some(after) => after.is_empty() || after.starts_with([' ', '\t']),
        None => false,
    }
}


fn collect_blockquote(ctx: &Ctx, first: &Line, to: usize) -> (MarkdownBlock, usize) {
    let src = ctx.source;
    let mut lines: Vec<Line> = vec![*first];
    let (first_skip, first_pad) = quote_prefix(first.text).unwrap_or((0, 0));
    let mut strips: Vec<(usize, usize, usize)> = vec![(first.start, first_skip, first_pad)];
    let mut pos = next_pos(first, to, src);
    while pos < to {
        let line = read_line(src, pos, to);
        if is_blank(line.text) {
            break;
        }
        if let Some((skip, pad)) = quote_prefix(line.text) {
            strips.push((line.start, skip, pad));
        } else if !is_paragraph_continuation(line.text) {
            break;
        }
        lines.push(line);
        pos = next_pos(&line, to, src);
    }
    // 引用块区间从首个内容字符起（不含行首空白）
    let bq_from = first.start + indent_prefix_len(first.text);
    let bq_to = lines.last().unwrap().end;
    let prefix_end = strips.first().map(|s| s.0 + s.1).unwrap_or(0);
    // 递归从行首起（strips 以行首为键；行首缩进由引用前缀一并剥掉）
    let mut children = recurse_container(ctx, strips, first.start, bq_to);
    // 续行 `>` 前缀剔除（贪婪逐行；只影响落在子块区间内的前缀）
    strip_quote_prefixes(&mut children, quote_line_prefixes(ctx.source, bq_from, bq_to), ctx.source);

    // callout：原始首行 `[!name]`（大小写不敏感）
    let line1 = &src[first.start..first.end];
    let mut callout: Option<String> = None;
    if let Some(c) = callout_of(&src[prefix_end..first.end]) {
        callout = Some(c);
        let lb = line1.find('[').unwrap_or(0);
        let rb = line1[lb..].find(']').map(|i| lb + i).unwrap_or(lb);
        if let Some(MarkdownBlock::Paragraph { inline, .. }) = children.first_mut() {
            let stripped = std::mem::take(inline);
            *inline = strip_ranges(
                stripped,
                &[RangeInfo { from: first.start + lb, to: first.start + rb + 1 }],
                ctx.source,
            );
        }
    }
    let block = MarkdownBlock::Blockquote { from: bq_from, to: bq_to, callout, children };
    (block, next_after(src, bq_to, to))
}

/// 引用块内每行的 `>` 前缀区间（贪婪：含前置缩进与标记后空格/嵌套 `>`）。
fn quote_line_prefixes(source: &str, from: usize, to: usize) -> Vec<RangeInfo> {
    let mut out = Vec::new();
    let mut line_start = from;
    while line_start < to {
        let line_end = source[line_start..to].find('\n').map(|i| line_start + i).unwrap_or(to);
        let greedy = greedy_quote_prefix(&source[line_start..line_end]);
        if let Some(w) = greedy {
            out.push(RangeInfo { from: line_start, to: line_start + w });
        }
        if line_end >= to {
            break;
        }
        line_start = line_end + 1;
    }
    out
}

fn greedy_quote_prefix(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if b.get(i) != Some(&b'>') {
        return None;
    }
    i += 1;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b'>') {
        i += 1;
    }
    Some(i)
}

/// 剔除落在含行内片段的子块区间内的引用前缀。
fn strip_quote_prefixes(children: &mut [MarkdownBlock], prefixes: Vec<RangeInfo>, source: &str) {
    if prefixes.is_empty() {
        return;
    }
    for child in children.iter_mut() {
        let (from, to, inline) = match child {
            MarkdownBlock::Paragraph { from, to, inline } => (*from, *to, inline),
            MarkdownBlock::Heading { from, to, inline, .. } => (*from, *to, inline),
            MarkdownBlock::FootnoteDef { from, to, inline, .. } => (*from, *to, inline),
            _ => continue,
        };
        let inner: Vec<RangeInfo> = prefixes
            .iter()
            .filter(|p| p.from >= from && p.to <= to)
            .cloned()
            .collect();
        if !inner.is_empty() {
            let taken = std::mem::take(inline);
            *inline = strip_ranges(taken, &inner, source);
        }
    }
}

/// callout 名（首行前缀之后）：`[!name]` 可选 +/- 尾随（`name` 限小写字母开头）。
fn callout_of(text: &str) -> Option<String> {
    let rest = text.strip_prefix("[!")?;
    let mut chars = rest.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let name_end = first.len_utf8()
        + chars
            .as_str()
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-')
            .count();
    let name = &rest[..name_end];
    rest[name_end..].strip_prefix(']').map(|_| name.to_ascii_lowercase())
}

// ===== 列表 =====

#[derive(Clone, Copy, PartialEq)]
enum ListKind {
    Bullet(u8),
    Ordered(u8),
}

impl ListKind {
    fn has_content(&self) -> bool {
        true
    }
}

fn list_kind(body: &str) -> Option<ListKind> {
    let b = body.as_bytes();
    match b.first()? {
        b'-' | b'+' | b'*' => {
            let after = &body[1..];
            let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
            if spaces == 0 && !after.is_empty() {
                return None;
            }
            Some(ListKind::Bullet(b[0]))
        }
        c if c.is_ascii_digit() => {
            let digits = b.iter().take_while(|x| x.is_ascii_digit()).count();
            if digits > 9 {
                return None;
            }
            let delim = *b.get(digits)?;
            if delim != b'.' && delim != b')' {
                return None;
            }
            let after = &body[digits + 1..];
            let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
            if spaces == 0 && !after.is_empty() {
                return None;
            }
            Some(ListKind::Ordered(delim))
        }
        _ => None,
    }
}

/// 项内容缩进宽 W（相对项标记列）：标记宽 + min(空格, 4)；空格 ≥1 时至少记 1。
fn item_content_width(marker_len: usize, spaces: usize) -> usize {
    if spaces == 0 {
        marker_len
    } else {
        marker_len + spaces.clamp(1, 4)
    }
}

fn collect_list(
    ctx: &Ctx,
    first: &Line,
    first_ind: usize,
    first_ind_cols: usize,
    kind: ListKind,
    to: usize,
) -> (MarkdownBlock, usize) {
    let src = ctx.source;
    let mut items: Vec<MarkdownListItem> = Vec::new();
    let mut pos = first.start;
    let mut list_to = first.end;

    'outer: while pos < to {
        let line = read_line(src, pos, to);
        if is_blank(line.text) {
            // 空行：下一非空行若为同类型项则续（松散列表），否则终止
            let mut peek = next_pos(&line, to, src);
            while peek < to {
                let l = read_line(src, peek, to);
                if is_blank(l.text) {
                    peek = next_pos(&l, to, src);
                    continue;
                }
                let body = trim_indent(l.text);
                if indent_columns(l.text) <= 3 && list_kind(body).is_some_and(|k| k == kind) {
                    pos = peek;
                    continue 'outer;
                }
                break 'outer;
            }
            break;
        }
        let (ind, ind_cols) = indent_prefix(line.text);
        let body = &line.text[ind..];
        let marker_len = marker_len_of(body, kind);
        let Some(marker_len) = (ind_cols <= 3).then_some(marker_len).flatten() else {
            break;
        };
        let spaces = marker_spaces_of(body, marker_len);
        let has_content = !body[marker_len + spaces..].trim().is_empty();
        if !has_content {
            // 空项不打断当前项流，但也构成独立项（ListItem 可为空）
            let item_to = line.end;
            items.push(MarkdownListItem {
                from: line.start + ind,
                to: item_to,
                task: false,
                checked: false,
                children: Vec::new(),
            });
            list_to = item_to;
            pos = next_pos(&line, to, src);
            continue;
        }
        // 收集项：首行 + 续行（缩进 ≥W 或 lazy）
        let width = first_ind_cols + item_content_width(marker_len, spaces);
        let item_from = line.start;
        let mark_from = line.start + ind;
        let content_off = marker_len + spaces;
        let (task, checked) = task_marker_of(&body[content_off..]);
        let mut strips: Vec<(usize, usize, usize)> = vec![(line.start, ind + content_off, 0)];
        let mut last = line.end;
        let mut j = next_pos(&line, to, src);
        while j < to {
            let l = read_line(src, j, to);
            if is_blank(l.text) {
                // 空行：peek 续行（缩进 ≥W）或同类型新项 → 空行属项内
                let mut peek = next_pos(&l, to, src);
                let mut continues = false;
                while peek < to {
                    let pl = read_line(src, peek, to);
                    if is_blank(pl.text) {
                        peek = next_pos(&pl, to, src);
                        continue;
                    }
                    continues = indent_columns(pl.text) >= width
                        || (indent_columns(pl.text) <= 3
                            && list_kind(trim_indent(pl.text)).is_some_and(|k| k == kind));
                    break;
                }
                if continues {
                    // 空行不计入项尾（项尾 = 最后一个内容行尾）
                    j = next_pos(&l, to, src);
                    continue;
                }
                break;
            }
            if indent_columns(l.text) >= width {
                let (skip, pad) = fold_indent(l.text, width);
                strips.push((l.start, skip, pad));
                last = l.end;
                j = next_pos(&l, to, src);
                continue;
            }
            if is_paragraph_continuation(l.text) {
                // lazy 续行：不剥
                last = l.end;
                j = next_pos(&l, to, src);
                continue;
            }
            break;
        }
        let item_to = last;
        let children_raw = recurse_container(ctx, strips, item_from, item_to);
        // 首块为段落时跳过（内容交给渲染层剥标记后行内渲染）
        let children = match children_raw.first() {
            Some(MarkdownBlock::Paragraph { .. }) => children_raw.into_iter().skip(1).collect(),
            _ => children_raw,
        };
        items.push(MarkdownListItem { from: mark_from, to: item_to, task, checked, children });
        list_to = item_to;
        pos = j;
    }
    let block = MarkdownBlock::List {
        ordered: matches!(kind, ListKind::Ordered(_)),
        from: first.start + first_ind,
        to: list_to,
        items,
    };
    (block, next_after(src, list_to, to))
}

/// 指定 kind 的标记宽（不匹配返回 None）。
fn marker_len_of(body: &str, kind: ListKind) -> Option<usize> {
    let b = body.as_bytes();
    match kind {
        ListKind::Bullet(c) => (b.first() == Some(&c)).then_some(1),
        ListKind::Ordered(delim) => {
            let digits = b.iter().take_while(|x| x.is_ascii_digit()).count();
            (b.get(digits) == Some(&delim)).then_some(digits + 1)
        }
    }
}

fn marker_spaces_of(body: &str, marker_len: usize) -> usize {
    let after = &body[marker_len..];
    after.len() - after.trim_start_matches([' ', '\t']).len()
}

/// 项内容首 `[ ]`/`[x]` + 空白或行尾 → 任务项。
fn task_marker_of(content: &str) -> (bool, bool) {
    let t = content.trim_start_matches([' ', '\t']);
    let b = t.as_bytes();
    if b.first() == Some(&b'[') && b.get(2) == Some(&b']') {
        let mark = b[1];
        if (mark == b' ' || mark == b'x' || mark == b'X')
            && b.get(3).is_none_or(|c| *c == b' ' || *c == b'\t')
        {
            return (true, mark != b' ');
        }
    }
    (false, false)
}

// ===== 水平分割线 =====

fn is_hr(body: &str) -> bool {
    let b = body.as_bytes();
    let first = match b.first() {
        Some(&c) if c == b'*' || c == b'-' || c == b'_' => c,
        _ => return false,
    };
    let mut count = 0;
    for &c in b {
        if c == first {
            count += 1;
        } else if c != b' ' && c != b'\t' && c != b'\r' {
            return false;
        }
    }
    count >= 3
}

// ===== 表格 =====

fn is_table_delim_row(body: &str) -> bool {
    let cells = split_row_text(body);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let t = c.trim();
            let t = t.strip_prefix(':').unwrap_or(t);
            let t = t.strip_suffix(':').unwrap_or(t);
            !t.is_empty() && t.bytes().all(|b| b == b'-')
        })
}

/// 虚拟字符：`\|` 占位为一个字符（源码占两字节）。
struct Vc {
    c: char,
    /// 源字节偏移（相对行文本）。
    off: usize,
    len: usize,
}

/// 切一行成单元格文本（`|` 分隔；`\|` 转义保留，行首尾 `|` 不计）。
fn split_row_text(text: &str) -> Vec<String> {
    vchars(text)
        .split(|v| v.c == '|')
        .map(|seg| {
            seg.iter()
                .map(|v| if v.len == 2 { "\\|".to_string() } else { v.c.to_string() })
                .collect()
        })
        .collect()
}

fn vchars(text: &str) -> Vec<Vc> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'|') {
            out.push(Vc { c: '\u{0}', off: i, len: 2 });
            i += 2;
        } else {
            let len = utf8_len(b[i]);
            out.push(Vc { c: text[i..i + len].chars().next().unwrap(), off: i, len });
            i += len;
        }
    }
    // 行首尾各一 `|` 不属内容
    if out.first().is_some_and(|v| v.c == '|') {
        out.remove(0);
    }
    if out.last().is_some_and(|v| v.c == '|') {
        out.pop();
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn collect_table(ctx: &Ctx, first: &Line, to: usize) -> (MarkdownBlock, usize) {
    let src = ctx.source;
    let mut pos = first.start;
    let mut lines: Vec<Line> = Vec::new();
    while pos < to {
        let line = read_line(src, pos, to);
        if is_blank(line.text) {
            break;
        }
        lines.push(line);
        pos = next_pos(&line, to, src);
    }
    // 行文本 trim 后的绝对起点
    let row_texts: Vec<(String, usize)> = lines
        .iter()
        .map(|l| {
            let t = l.text.trim();
            (t.to_string(), l.start + (l.text.len() - t.len()))
        })
        .collect();
    let header = table_cells(&row_texts[0].0, row_texts[0].1);
    let aligns: Vec<String> = row_texts
        .get(1)
        .map(|(t, _)| {
            split_row_text(t)
                .iter()
                .map(|c| {
                    let t = c.trim();
                    if t.starts_with(':') && t.ends_with(':') && t.len() > 1 {
                        "center".to_string()
                    } else if t.ends_with(':') && t.len() > 1 {
                        "right".to_string()
                    } else if t.starts_with(':') && t.len() > 1 {
                        "left".to_string()
                    } else {
                        String::new()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let rows: Vec<Vec<MarkdownTableCell>> = row_texts[2..]
        .iter()
        .map(|(t, off)| table_cells(t, *off))
        .collect();
    let last = lines.last().unwrap();
    let block = MarkdownBlock::Table {
        from: first.start + indent_prefix_len(first.text),
        to: last.end,
        header,
        aligns,
        rows,
    };
    (block, next_after(src, last.end, to))
}

/// 单元格源区间（源区间承载、首尾空白不计、
/// 末字符为 `\|` 时区间终点越过后一字节）。
fn table_cells(text: &str, abs_start: usize) -> Vec<MarkdownTableCell> {
    let v = vchars(text);
    let mut out: Vec<MarkdownTableCell> = Vec::new();
    let mut seg: Vec<&Vc> = Vec::new();
    for vc in &v {
        if vc.c == '|' {
            out.push(cell_range(&seg, abs_start));
            seg.clear();
        } else {
            seg.push(vc);
        }
    }
    out.push(cell_range(&seg, abs_start));
    out
}

fn cell_range(seg: &[&Vc], abs_start: usize) -> MarkdownTableCell {
    let is_space = |v: &Vc| v.len == 1 && (v.c == ' ' || v.c == '\t');
    let mut lo = 0;
    let mut hi = seg.len();
    while lo < hi && is_space(seg[lo]) {
        lo += 1;
    }
    while hi > lo && is_space(seg[hi - 1]) {
        hi -= 1;
    }
    if lo >= hi {
        let at = seg.first().map(|v| v.off).unwrap_or(0);
        let p = abs_start + at;
        return MarkdownTableCell { from: p, to: p };
    }
    let first = seg[lo];
    let last = seg[hi - 1];
    MarkdownTableCell {
        from: abs_start + first.off,
        to: abs_start + last.off + last.len,
    }
}

// ===== 缩进代码 =====

fn collect_indented_code(ctx: &Ctx, first: &Line, to: usize) -> (MarkdownBlock, usize) {
    let src = ctx.source;
    let from = first.start + consume_columns(first.text, 4).0;
    let mut to_b = first.end;
    let mut code = strip_indent_4(first.text).to_string();
    let mut pos = next_pos(first, to, src);
    while pos < to {
        let line = read_line(src, pos, to);
        if is_blank(line.text) {
            // 空行：peek 下一非空行，缩进 >=4 则续（空行计入内容），否则终止（尾部空行不属块）
            let mut peek = next_pos(&line, to, src);
            let mut cont = false;
            while peek < to {
                let pl = read_line(src, peek, to);
                if is_blank(pl.text) {
                    peek = next_pos(&pl, to, src);
                    continue;
                }
                cont = indent_columns(pl.text) >= 4;
                break;
            }
            if !cont {
                break;
            }
            code.push('\n'); // 空行与上一行之间的换行
            to_b = line.end;
            pos = next_pos(&line, to, src);
            continue;
        }
        if indent_columns(line.text) < 4 {
            break;
        }
        code.push('\n');
        code.push_str(strip_indent_4(line.text));
        to_b = line.end;
        pos = next_pos(&line, to, src);
    }
    let block = MarkdownBlock::IndentedCode { from, to: to_b, code };
    (block, pos)
}

/// 剥行首 4 列缩进（不足 4 列时整行剥全部缩进）。
fn strip_indent_4(text: &str) -> &str {
    &text[consume_columns(text, 4).0..]
}

// ===== 段落 / 脚注 / Setext / 数学 =====

fn collect_paragraph(
    ctx: &Ctx,
    first: &Line,
    to: usize,
    math: Option<&mut MathState>,
) -> (Vec<MarkdownBlock>, usize) {
    let src = ctx.source;
    // 收集段落行（Setext 下划线终止成标题）
    let mut lines: Vec<Line> = vec![*first];
    let mut pos = next_pos(first, to, src);
    let mut heading: Option<u8> = None;
    while pos < to {
        let line = read_line(src, pos, to);
        if is_blank(line.text) {
            break;
        }
        let body = trim_indent(line.text);
        if indent_columns(line.text) <= 3 {
            let t = body.trim_end();
            if !t.is_empty() && (t.bytes().all(|c| c == b'=') || t.bytes().all(|c| c == b'-')) {
                heading = Some(if t.as_bytes()[0] == b'=' { 1 } else { 2 });
                lines.push(line);
                pos = next_pos(&line, to, src);
                break;
            }
        }
        if !is_paragraph_continuation(line.text) {
            break;
        }
        // GFM 表格可打断段落
        if body.contains('|') {
            if let Some(nl) = peek_next_line(src, &line, to) {
                if is_table_delim_row(trim_indent(nl.text)) {
                    break;
                }
            }
        }
        lines.push(line);
        pos = next_pos(&line, to, src);
    }
    // 段落区间从首字符起（不含行首空白）
    let para_from = lines[0].start + indent_prefix_len(lines[0].text);
    let para_end = lines.last().unwrap().end;

    if let Some(level) = heading {
        // 内容 = 下划线行之前的标题文本行
        let content_end = lines[lines.len() - 2].end;
        let inline = inline::parse_inline_range(ctx, para_from, content_end);
        return (
            vec![MarkdownBlock::Heading { level, from: para_from, to: para_end, inline }],
            pos,
        );
    }
    // 脚注定义：首行 `[^label]: text`（只覆盖定义行）
    if let Some(f) = footnote_def(ctx, first) {
        let fn_next = next_pos(first, to, src);
        let mut blocks = vec![f];
        if para_end > fn_next {
            blocks.push(paragraph_block(ctx, fn_next, para_end));
        }
        return (blocks, pos);
    }
    // 数学区间消费
    if let Some(st) = math {
        if st.ranges.iter().any(|r| para_from < r.to && para_end > r.from) {
            let MathState { ranges, used } = st;
            let segs = paragraph_segments(ctx, para_from, para_end, ranges, used);
            return (segs, pos);
        }
    }
    // LinkReference 形态段落：URL 部分保持纯文本（不产裸 URL autolink）
    if is_link_ref_def_line(lines[0].text) {
        let ctx2 = Ctx { source: ctx.source, options: ctx.options, no_bare_autolink: true };
        let inline = inline::parse_inline_range(&ctx2, para_from, para_end);
        return (vec![MarkdownBlock::Paragraph { from: para_from, to: para_end, inline }], pos);
    }
    (vec![paragraph_block(ctx, para_from, para_end)], pos)
}

fn paragraph_block(ctx: &Ctx, from: usize, to: usize) -> MarkdownBlock {
    let inline = inline::parse_inline_range(ctx, from, to);
    MarkdownBlock::Paragraph { from, to, inline }
}

/// 数学区间 → mathBlock；空数学体降级为原文段落（源文本不从渲染面消失）。
fn math_block_of(ctx: &Ctx, r: RangeInfo) -> MarkdownBlock {
    let raw = &ctx.source[r.from..r.to];
    let inner = raw.trim_start_matches("$$").trim_end_matches("$$").trim();
    if !inner.is_empty() {
        return MarkdownBlock::MathBlock { from: r.from, to: r.to, tex: inner.to_string() };
    }
    let inline = inline::parse_inline_range(ctx, r.from, r.to);
    MarkdownBlock::Paragraph { from: r.from, to: r.to, inline }
}

/// 段落按交叠数学区间切分（未消费区间依次
/// 消费为 mathBlock，区间外残余保持段落，跨段区间只跳过本段覆盖部分）。
fn paragraph_segments(
    ctx: &Ctx,
    from: usize,
    to: usize,
    math_ranges: &[RangeInfo],
    used: &mut HashSet<usize>,
) -> Vec<MarkdownBlock> {
    let mut out = Vec::new();
    let mut cursor = from;
    for (i, r) in math_ranges.iter().enumerate() {
        if r.to <= cursor {
            continue;
        }
        if r.from >= to {
            break;
        }
        if used.contains(&i) {
            cursor = cursor.max(r.to);
            if cursor >= to {
                break;
            }
            continue;
        }
        if r.from > cursor {
            out.push(paragraph_block(ctx, cursor, r.from));
        }
        used.insert(i);
        out.push(math_block_of(ctx, *r));
        cursor = cursor.max(r.to);
        if cursor >= to {
            break;
        }
    }
    if cursor < to {
        out.push(paragraph_block(ctx, cursor, to));
    }
    out
}

/// 段落首行 `[^label]: text`（只覆盖定义行；行尾 `\r` 不属定义内容）。
fn footnote_def(ctx: &Ctx, first: &Line) -> Option<MarkdownBlock> {
    let raw = first.text.strip_suffix('\r').unwrap_or(first.text);
    let ind = indent_prefix_len(raw);
    let base = first.start + ind;
    let line = &raw[ind..];
    let rest = line.strip_prefix("[^")?;
    let rb = rest.find(']')?;
    let label = &rest[..rb];
    if label.is_empty() {
        return None;
    }
    let after = rest[rb + 1..].strip_prefix(':')?;
    let content = after.trim_start_matches([' ', '\t']);
    let text_from = base + (line.len() - content.len());
    let ctx2 = Ctx { source: ctx.source, options: ctx.options, no_bare_autolink: true };
    let inline = inline::parse_inline_range(&ctx2, text_from, base + line.len());
    Some(MarkdownBlock::FootnoteDef {
        from: base,
        to: base + line.len(),
        label: label.to_string(),
        inline,
    })
}

// ===== 容器递归（等宽虚拟缓冲）=====

/// 复制 [from,to) 并把 strips 区间替换为等宽空格，递归解析后平移回原偏移。
/// 容器内容的偏移映射：buf 偏移 → 原偏移（逐行段记录，delta = 原起点 - buf 起点）。
struct OffsetMap {
    segments: Vec<(usize, usize, isize)>,
}

impl OffsetMap {
    fn map(&self, off: usize) -> usize {
        // 左闭右开：段尾 buf_to 属下一段（行尾换行归属下一行首前位置由最后段兜底）
        for (buf_from, buf_to, delta) in &self.segments {
            if off >= *buf_from && off < *buf_to {
                return (off as isize + delta) as usize;
            }
        }
        self.segments.last().map(|(_, buf_to, delta)| (*buf_to as isize + delta) as usize).unwrap_or(off)
    }
}

/// 容器递归：每行剥前缀（行首剩余空白折成空格）后拼接重解析，块区间经映射回原空间。
/// 容器内不走文档级数学区间消费：内层按重解析文本独立成块。
fn recurse_container(
    ctx: &Ctx,
    strips: Vec<(usize, usize, usize)>,
    from: usize,
    to: usize,
) -> Vec<MarkdownBlock> {
    let src = ctx.source;
    let mut buf = String::new();
    let mut map: Vec<(usize, usize, isize)> = Vec::new();
    let mut pos = from;
    while pos < to {
        let line = read_line(src, pos, to);
        let (skip, pad) = strips
            .iter()
            .find(|(s, _, _)| *s == line.start)
            .map(|(_, skip, pad)| (*skip, *pad))
            .unwrap_or((0, 0));
        let content_start = line.start + skip.min(line.end - line.start);
        let buf_from = buf.len();
        for _ in 0..pad {
            buf.push(' ');
        }
        buf.push_str(&src[content_start..line.end]);
        if line.end < to {
            buf.push('\n');
        }
        // 补入的空格不对应源文本：内容起点为锚，避免把它算进平移量
        map.push((buf_from, buf.len(), content_start as isize - (buf_from + pad) as isize));
        pos = next_pos(&line, to, src);
    }
    let inner = Ctx { source: &buf, options: ctx.options, no_bare_autolink: false };
    let blocks = parse_blocks(&inner, 0, buf.len(), None);
    let om = OffsetMap { segments: map };
    blocks.into_iter().map(|b| map_block(b, &om)).collect()
}

fn map_block(b: MarkdownBlock, om: &OffsetMap) -> MarkdownBlock {
    use MarkdownBlock::*;
    match b {
        Heading { level, from, to, inline } => Heading {
            level,
            from: om.map(from),
            to: om.map(to),
            inline: map_spans(inline, om),
        },
        Paragraph { from, to, inline } => Paragraph {
            from: om.map(from),
            to: om.map(to),
            inline: map_spans(inline, om),
        },
        Blockquote { from, to, callout, children } => Blockquote {
            from: om.map(from),
            to: om.map(to),
            callout,
            children: children.into_iter().map(|c| map_block(c, om)).collect(),
        },
        List { ordered, from, to, items } => List {
            ordered,
            from: om.map(from),
            to: om.map(to),
            items: items
                .into_iter()
                .map(|it| MarkdownListItem {
                    from: om.map(it.from),
                    to: om.map(it.to),
                    task: it.task,
                    checked: it.checked,
                    children: it.children.into_iter().map(|c| map_block(c, om)).collect(),
                })
                .collect(),
        },
        FencedCode { from, to, lang, code, content_from, content_to } => FencedCode {
            from: om.map(from),
            to: om.map(to),
            lang,
            code,
            content_from: om.map(content_from),
            content_to: om.map(content_to),
        },
        IndentedCode { from, to, code } => IndentedCode { from: om.map(from), to: om.map(to), code },
        Hr { from, to } => Hr { from: om.map(from), to: om.map(to) },
        Table { from, to, header, aligns, rows } => Table {
            from: om.map(from),
            to: om.map(to),
            header: map_cells(header, om),
            aligns,
            rows: rows.into_iter().map(|row| map_cells(row, om)).collect(),
        },
        MathBlock { from, to, tex } => MathBlock { from: om.map(from), to: om.map(to), tex },
        HtmlBlock { from, to, html } => HtmlBlock { from: om.map(from), to: om.map(to), html },
        FootnoteDef { from, to, label, inline } => FootnoteDef {
            from: om.map(from),
            to: om.map(to),
            label,
            inline: map_spans(inline, om),
        },
    }
}

fn map_cells(cells: Vec<MarkdownTableCell>, om: &OffsetMap) -> Vec<MarkdownTableCell> {
    cells
        .into_iter()
        .map(|c| MarkdownTableCell { from: om.map(c.from), to: om.map(c.to) })
        .collect()
}

fn map_spans(spans: Vec<InlineSpan>, om: &OffsetMap) -> Vec<InlineSpan> {
    spans
        .into_iter()
        .map(|s| match s {
            InlineSpan::Text { from, to, text } => InlineSpan::Text { from: om.map(from), to: om.map(to), text },
            InlineSpan::Strong { from, to, children } => InlineSpan::Strong { from: om.map(from), to: om.map(to), children: map_spans(children, om) },
            InlineSpan::Em { from, to, children } => InlineSpan::Em { from: om.map(from), to: om.map(to), children: map_spans(children, om) },
            InlineSpan::Strike { from, to, children } => InlineSpan::Strike { from: om.map(from), to: om.map(to), children: map_spans(children, om) },
            InlineSpan::Highlight { from, to, children } => InlineSpan::Highlight { from: om.map(from), to: om.map(to), children: map_spans(children, om) },
            InlineSpan::Code { from, to, text } => InlineSpan::Code { from: om.map(from), to: om.map(to), text },
            InlineSpan::MathInline { from, to, tex } => InlineSpan::MathInline { from: om.map(from), to: om.map(to), tex },
            InlineSpan::Comment { from, to } => InlineSpan::Comment { from: om.map(from), to: om.map(to) },
            InlineSpan::Link { from, to, href, label, form, title } => InlineSpan::Link { from: om.map(from), to: om.map(to), href, label, form, title },
            InlineSpan::Autolink { from, to, href, label } => InlineSpan::Autolink { from: om.map(from), to: om.map(to), href, label },
            InlineSpan::Wiki { from, to, target, label } => InlineSpan::Wiki { from: om.map(from), to: om.map(to), target, label },
            InlineSpan::Image { from, to, src, alt, width, height, title } => InlineSpan::Image { from: om.map(from), to: om.map(to), src, alt, width, height, title },
            InlineSpan::Tag { from, to, tag } => InlineSpan::Tag { from: om.map(from), to: om.map(to), tag },
            InlineSpan::FootnoteRef { from, to, label } => InlineSpan::FootnoteRef { from: om.map(from), to: om.map(to), label },
            InlineSpan::Mention { from, to, key, label, trigger } => InlineSpan::Mention { from: om.map(from), to: om.map(to), key, label, trigger },
            InlineSpan::Html { from, to, html } => InlineSpan::Html { from: om.map(from), to: om.map(to), html },
            InlineSpan::HardBreak { from, to } => InlineSpan::HardBreak { from: om.map(from), to: om.map(to) },
        })
        .collect()
}

// ===== 区间裁剪（callout/引用前缀剔除）=====

pub(super) fn subtract(from: usize, to: usize, excludes: &[RangeInfo]) -> Vec<RangeInfo> {
    let mut cuts: Vec<&RangeInfo> = excludes.iter().filter(|e| e.to > from && e.from < to).collect();
    cuts.sort_by_key(|e| e.from);
    let mut out = Vec::new();
    let mut cursor = from;
    for c in cuts {
        let stop = c.from.min(to);
        if stop > cursor {
            out.push(RangeInfo { from: cursor, to: stop });
        }
        cursor = cursor.max(c.to.min(to));
    }
    if cursor < to {
        out.push(RangeInfo { from: cursor, to });
    }
    out
}

/// 从行内片段集中抠掉指定区间（只影响文本片段）。
pub(super) fn strip_ranges(spans: Vec<InlineSpan>, excludes: &[RangeInfo], source: &str) -> Vec<InlineSpan> {
    if excludes.is_empty() {
        return spans;
    }
    let mut out = Vec::new();
    for s in spans {
        match s {
            InlineSpan::Text { from, to, .. } => {
                for chunk in subtract(from, to, excludes) {
                    out.push(InlineSpan::Text {
                        from: chunk.from,
                        to: chunk.to,
                        text: source[chunk.from..chunk.to].to_string(),
                    });
                }
            }
            other => out.push(other),
        }
    }
    out
}

// ===== 块级数学预扫 =====

fn block_math_ranges(source: &str) -> Vec<RangeInfo> {
    let mut out = Vec::new();
    let lines: Vec<&str> = source.split('\n').collect();
    let mut line_starts: Vec<usize> = Vec::with_capacity(lines.len());
    let mut off = 0usize;
    for line in &lines {
        line_starts.push(off);
        off += line.len() + 1;
    }
    let mut i = 0;
    while i < lines.len() {
        let text = lines[i];
        let ind = indent_prefix_len(text);
        let open_idx = if text[ind..].starts_with("$$") { ind } else { usize::MAX };
        if open_idx == usize::MAX {
            i += 1;
            continue;
        }
        let rest = &text[open_idx + 2..];
        if let Some(close) = rest.find("$$") {
            // 同行闭合（`$$...$$`）
            let from = line_starts[i] + open_idx;
            let to = line_starts[i] + open_idx + 2 + close + 2;
            out.push(RangeInfo { from, to });
            i += 1;
            continue;
        }
        // 跨行：从 `$$` 起始行到首个含 `$$` 的行
        let mut j = i + 1;
        let mut closed = false;
        while j < lines.len() {
            if let Some(close) = lines[j].find("$$") {
                let from = line_starts[i] + open_idx;
                let to = line_starts[j] + close + 2;
                out.push(RangeInfo { from, to });
                i = j + 1;
                closed = true;
                break;
            }
            j += 1;
        }
        if !closed {
            break;
        }
    }
    out
}

pub(super) fn block_from(b: &MarkdownBlock) -> usize {
    match b {
        MarkdownBlock::Heading { from, .. }
        | MarkdownBlock::Paragraph { from, .. }
        | MarkdownBlock::Blockquote { from, .. }
        | MarkdownBlock::List { from, .. }
        | MarkdownBlock::FencedCode { from, .. }
        | MarkdownBlock::IndentedCode { from, .. }
        | MarkdownBlock::Hr { from, .. }
        | MarkdownBlock::Table { from, .. }
        | MarkdownBlock::MathBlock { from, .. }
        | MarkdownBlock::HtmlBlock { from, .. }
        | MarkdownBlock::FootnoteDef { from, .. } => *from,
    }
}

pub(super) fn block_to(b: &MarkdownBlock) -> usize {
    match b {
        MarkdownBlock::Heading { to, .. }
        | MarkdownBlock::Paragraph { to, .. }
        | MarkdownBlock::Blockquote { to, .. }
        | MarkdownBlock::List { to, .. }
        | MarkdownBlock::FencedCode { to, .. }
        | MarkdownBlock::IndentedCode { to, .. }
        | MarkdownBlock::Hr { to, .. }
        | MarkdownBlock::Table { to, .. }
        | MarkdownBlock::MathBlock { to, .. }
        | MarkdownBlock::HtmlBlock { to, .. }
        | MarkdownBlock::FootnoteDef { to, .. } => *to,
    }
}
