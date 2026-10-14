//! 行内解析：结构原子扫描（行内代码/链接/图片/autolink/HTML 标签/实体/转义/强调）+
//! 按行正则补充（注释/高亮/wiki/脚注引用/行内数学/mention/标签）+ 候选合并，
//! 语义由对拍快照 `fixtures/markdown/parse.json` 锁定。

use super::parse::Ctx;
use super::types::{InlineSpan, LinkForm, MentionCandidate, ParseOptions, RangeInfo};
use regex::Regex;
use std::sync::OnceLock;

pub(super) fn parse_inline_range(ctx: &Ctx, from: usize, to: usize) -> Vec<InlineSpan> {
    if to <= from {
        return Vec::new();
    }
    let mut atoms = scan_atoms(ctx, from, to);
    let mut candidates = scan_line_matches(ctx, from, to);
    candidates.sort_by(|a, b| a.from.cmp(&b.from).then(b.to.cmp(&a.to)));

    let mut accepted: Vec<Atom> = Vec::new();
    for c in candidates {
        if c.from < from || c.to > to {
            continue;
        }
        if accepted.iter().any(|a| c.from < a.to && c.to > a.from) {
            continue;
        }
        if atoms.iter().any(|a| c.from < a.to && c.to > a.from) {
            continue;
        }
        accepted.push(c);
    }

    atoms.extend(accepted);
    atoms.sort_by(|a, b| a.from.cmp(&b.from).then(a.to.cmp(&b.to)));
    let mut spans: Vec<InlineSpan> = Vec::new();
    let mut pos = from;
    for e in atoms {
        if e.from > pos {
            push_text(&mut spans, ctx.source, pos, e.from);
        }
        spans.push(e.span);
        if e.to > pos {
            pos = e.to;
        }
    }
    if pos < to {
        push_text(&mut spans, ctx.source, pos, to);
    }
    spans
}

#[derive(Clone)]
struct Atom {
    from: usize,
    to: usize,
    span: InlineSpan,
}

fn push_text(out: &mut Vec<InlineSpan>, source: &str, from: usize, to: usize) {
    out.push(InlineSpan::Text { from, to, text: source[from..to].to_string() });
}

// ===== 结构原子扫描 =====

fn scan_atoms(ctx: &Ctx, from: usize, to: usize) -> Vec<Atom> {
    let src = ctx.source;
    let bytes = src.as_bytes();
    let mut delims: Vec<Delim> = Vec::new();
    let mut atoms: Vec<Atom> = Vec::new();
    // HTMLTag 配对成组（开标签起、配对深度归零止）
    let mut html_group: Vec<Atom> = Vec::new();
    let mut html_depth = 0usize;

    let flush_html = |group: &mut Vec<Atom>, depth: &mut usize, atoms: &mut Vec<Atom>| {
        if let (Some(first), Some(last)) = (group.first(), group.last()) {
            let (from, to) = (first.from, last.to);
            atoms.push(Atom {
                from,
                to,
                span: InlineSpan::Html { from, to, html: src[from..to].to_string() },
            });
        }
        group.clear();
        *depth = 0;
    };

    let mut i = from;
    while i < to {
        match bytes[i] {
            b'`' => {
                let run = run_len(bytes, i, to, b'`');
                match close_backtick_run(bytes, i + run, to, run) {
                    Some(close_start) => {
                        let content = &src[i + run..close_start];
                        atoms.push(Atom {
                            from: i,
                            to: close_start + run,
                            span: InlineSpan::Code {
                                from: i,
                                to: close_start + run,
                                text: inline_code_text(content).to_string(),
                            },
                        });
                        i = close_start + run;
                    }
                    None => {
                        i += run; // 字面反引号
                    }
                }
            }
            b'\\' => {
                if i + 1 < to && is_ascii_punct(bytes[i + 1]) {
                    atoms.push(Atom {
                        from: i,
                        to: i + 2,
                        span: InlineSpan::Text {
                            from: i,
                            to: i + 2,
                            text: src[i + 1..i + 2].to_string(),
                        },
                    });
                    i += 2;
                } else {
                    i += 1;
                }
            }
            b'<' => {
                match scan_lt(ctx, i, to) {
                    LtScan::Autolink(end) => {
                        let inner = &src[i + 1..end - 1];
                        atoms.push(Atom {
                            from: i,
                            to: end,
                            span: InlineSpan::Autolink {
                                from: i,
                                to: end,
                                href: inner.to_string(),
                                label: inner.to_string(),
                            },
                        });
                        i = end;
                    }
                    LtScan::HtmlTag(end) => {
                        let raw = &src[i..end];
                        let atom = Atom {
                            from: i,
                            to: end,
                            span: InlineSpan::Html { from: i, to: end, html: raw.to_string() },
                        };
                        if raw.starts_with("<!--") {
                            // 行内 HTML 注释与 `%%注释%%` 同语义：承载内容但不渲染
                            flush_html(&mut html_group, &mut html_depth, &mut atoms);
                            atoms.push(Atom { from: i, to: end, span: InlineSpan::Comment { from: i, to: end } });
                            i = end;
                            continue;
                        }
                        if raw.starts_with("</") {
                            if html_depth == 0 {
                                // 游离闭标签：自成一组
                                flush_html(&mut html_group, &mut html_depth, &mut atoms);
                                html_group.push(atom);
                                flush_html(&mut html_group, &mut html_depth, &mut atoms);
                            } else {
                                html_group.push(atom);
                                html_depth -= 1;
                                if html_depth == 0 {
                                    flush_html(&mut html_group, &mut html_depth, &mut atoms);
                                }
                            }
                        } else {
                            html_group.push(atom);
                            if raw.ends_with("/>") && html_depth == 0 {
                                flush_html(&mut html_group, &mut html_depth, &mut atoms);
                            } else if !raw.ends_with("/>") {
                                html_depth += 1;
                            }
                        }
                        i = end;
                    }
                    LtScan::Comment(end) => {
                        flush_html(&mut html_group, &mut html_depth, &mut atoms);
                        atoms.push(Atom { from: i, to: end, span: InlineSpan::Comment { from: i, to: end } });
                        i = end;
                    }
                    LtScan::None => {
                        i += 1;
                    }
                }
            }
            b'&' => {
                match scan_entity(bytes, i, to) {
                    Some(end) => {
                        atoms.push(Atom {
                            from: i,
                            to: end,
                            span: InlineSpan::Html { from: i, to: end, html: src[i..end].to_string() },
                        });
                        i = end;
                    }
                    None => i += 1,
                }
            }
            b'!' if bytes.get(i + 1) == Some(&b'[') => {
                match scan_link(ctx, i + 1, to, true) {
                    Some((end, mut span)) => {
                        set_span_range(&mut span, i, end);
                        atoms.push(Atom { from: i, to: end, span });
                        i = end;
                    }
                    None => i += char_len_at(bytes, i),
                }
            }
            b'[' => match scan_link(ctx, i, to, false) {
                Some((end, span)) => {
                    atoms.push(Atom { from: i, to: end, span });
                    i = end;
                }
                None => i += 1,
            },
            b'*' | b'_' | b'~' => {
                let ch = bytes[i];
                let run = run_len(bytes, i, to, ch);
                let before = if i > from { src[..i].chars().next_back() } else { None };
                let after = src[i + run..].chars().next();
                let (can_open, can_close) = flanking(before, after, ch);
                delims.push(Delim {
                    ch,
                    start: i,
                    end: i + run,
                    count: run,
                    can_open,
                    can_close,
                });
                i += run;
            }
            b'\n' => {
                // GFM 硬换行：行尾 ≥2 空格
                let mut spaces = 0;
                while i > from && bytes[i - 1 - spaces] == b' ' {
                    spaces += 1;
                }
                if spaces >= 2 {
                    let hb_from = i - spaces;
                    atoms.push(Atom {
                        from: hb_from,
                        to: i + 1,
                        span: InlineSpan::HardBreak { from: hb_from, to: i + 1 },
                    });
                }
                i += 1;
            }
            _ => {
                let end = if ctx.no_bare_autolink {
                    None
                } else {
                    bare_autolink_end(src, i, to)
                };
                if let Some(end) = end {
                    let text = src[i..end].to_string();
                    atoms.push(Atom {
                        from: i,
                        to: end,
                        span: InlineSpan::Autolink { from: i, to: end, href: text.clone(), label: text },
                    });
                    i = end;
                } else {
                    i += char_len_at(bytes, i);
                }
            }
        }
    }
    flush_html(&mut html_group, &mut html_depth, &mut atoms);

    // 强调配对
    let mut containers = process_emphasis(ctx, &mut delims, from, to);
    // 容器内极大化：嵌套强调的外层保留，内层由 children 携带
    containers.sort_by(|a, b| a.from.cmp(&b.from).then(b.to.cmp(&a.to)));
    let mut maximal: Vec<Atom> = Vec::new();
    for c in containers {
        let contained = maximal
            .iter()
            .any(|m| m.from <= c.from && c.to <= m.to && !(m.from == c.from && m.to == c.to));
        if !contained {
            maximal.push(c);
        }
    }
    // 被容器包含的其他原子剔除（children 由容器递归重现）
    atoms.retain(|a| !maximal.iter().any(|c| c.from <= a.from && a.to <= c.to));
    atoms.extend(maximal);
    atoms
}

/// 裸 URL autolink（GFM 扩展）：http(s):// 或 www. 起，非空白连续，
/// 前导须行首/空白/(*_~，末尾标点按 GFM 规则剥离。
fn bare_autolink_end(src: &str, i: usize, to: usize) -> Option<usize> {
    let rest = &src[i..to];
    let after = if i > 0 { src[..i].chars().next_back() } else { None };
    let ok_precede = after.is_none_or(|c| c.is_whitespace() || matches!(c, '*' | '_' | '~' | '('));
    if !ok_precede {
        return None;
    }
    let scheme_len = if rest.starts_with("http://") || rest.starts_with("https://") {
        rest.find("://").unwrap() + 3
    } else if rest.starts_with("www.") {
        4
    } else {
        return None;
    };
    let b = rest.as_bytes();
    let mut end = scheme_len;
    while end < b.len() && !b[end].is_ascii_whitespace() && b[end] != b'<' {
        end += 1;
    }
    // 末尾标点剥离（GFM）
    loop {
        match b.get(end.wrapping_sub(1)) {
            Some(b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~' | b'\'' | b'"' | b';') => end -= 1,
            Some(b')') => {
                let opens = rest[..end].matches('(').count();
                let closes = rest[..end].matches(')').count();
                if closes > opens { end -= 1; } else { break; }
            }
            _ => break,
        }
    }
    if end <= scheme_len {
        return None;
    }
    Some(i + end)
}

/// i 处字符的字节宽（多字节安全前进用）。
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn char_len_at(bytes: &[u8], i: usize) -> usize {
    utf8_len(bytes[i])
}

fn run_len(bytes: &[u8], pos: usize, to: usize, ch: u8) -> usize {
    let mut n = 0;
    while pos + n < to && bytes[pos + n] == ch {
        n += 1;
    }
    n
}

/// 找到下一个等长反引号 run 的起点。
fn close_backtick_run(bytes: &[u8], from: usize, to: usize, run: usize) -> Option<usize> {
    let mut i = from;
    while i < to {
        if bytes[i] == b'`' {
            let n = run_len(bytes, i, to, b'`');
            if n == run {
                return Some(i);
            }
            i += n;
        } else {
            i += char_len_at(bytes, i);
        }
    }
    None
}

/// 行内代码内容：剥定界反引号（首尾空格保留）。
fn inline_code_text(raw: &str) -> &str {
    let b = raw.as_bytes();
    if b.first() != Some(&b'`') {
        return raw;
    }
    let lead = run_len(b, 0, raw.len(), b'`');
    let mut tail = 0;
    while tail < raw.len() && b[raw.len() - 1 - tail] == b'`' {
        tail += 1;
    }
    if lead == tail && lead * 2 <= raw.len() {
        &raw[lead..raw.len() - tail]
    } else {
        raw
    }
}

enum LtScan {
    Autolink(usize),
    HtmlTag(usize),
    Comment(usize),
    None,
}

fn scan_lt(ctx: &Ctx, i: usize, to: usize) -> LtScan {
    let src = ctx.source;
    let rest = &src[i..to];
    if let Some(end) = rest.find("-->") {
        return LtScan::Comment(i + end + 3);
    }
    // autolink：<scheme:...>（CommonMark 绝对 URI）
    if let Some(end) = autolink_end(rest) {
        return LtScan::Autolink(i + end);
    }
    if let Some(end) = html_tag_end(rest) {
        return LtScan::HtmlTag(i + end);
    }
    LtScan::None
}

fn autolink_end(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    if b.len() < 3 || !b[1].is_ascii_alphabetic() {
        return None;
    }
    let colon = rest[1..].find(':')? + 1;
    if colon > 32 || !b[1..colon - 1].iter().all(|&c| c.is_ascii_alphanumeric() || c == b'+' || c == b'.' || c == b'-') {
        return None;
    }
    let mut end = colon;
    while end < b.len() && !b[end].is_ascii_whitespace() && b[end] != b'<' && b[end] != b'>' {
        end += 1;
    }
    if end < b.len() && b[end] == b'>' {
        Some(end + 1)
    } else {
        None
    }
}

/// HTML 行内标签结束位置（属性宽松校验：引号内字符任意，引号外不含空白/`>`/`<`）。
fn html_tag_end(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    let mut i = 1;
    let closing = b.get(1) == Some(&b'/');
    if closing {
        i = 2;
    }
    if !b.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    i += 1;
    i += b[i..].iter().take_while(|&&c| c.is_ascii_alphanumeric() || c == b'-').count();
    if closing {
        // `</tag>` 允许尾随空白
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        return (b.get(i) == Some(&b'>')).then(|| i + 1);
    }
    // 属性循环
    loop {
        // 跳过空白
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        match b.get(i) {
            Some(b'>') => return Some(i + 1),
            Some(b'/') if b.get(i + 1) == Some(&b'>') => return Some(i + 2),
            None => return None,
            Some(c) if c.is_ascii_alphabetic() || *c == b'_' || *c == b':' => {
                i += 1;
                i += b[i..]
                    .iter()
                    .take_while(|&&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'.' | b'-'))
                    .count();
                // 可选 = value
                let save = i;
                while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                if b.get(i) == Some(&b'=') {
                    i += 1;
                    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                        i += 1;
                    }
                    match b.get(i) {
                        Some(b'"') => {
                            let close = rest[i + 1..].find('"')? + i + 1;
                            i = close + 1;
                        }
                        Some(b'\'') => {
                            let close = rest[i + 1..].find('\'')? + i + 1;
                            i = close + 1;
                        }
                        Some(&c) if !c.is_ascii_whitespace() && !matches!(c, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`') => {
                            i += b[i..]
                                .iter()
                                .take_while(|&&c| {
                                    !c.is_ascii_whitespace() && !matches!(c, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
                                })
                                .count();
                        }
                        _ => {
                            i = save;
                        }
                    }
                } else {
                    i = save;
                }
            }
            _ => return None,
        }
    }
}

fn scan_entity(bytes: &[u8], i: usize, to: usize) -> Option<usize> {
    let mut j = i + 1;
    if bytes.get(j) == Some(&b'#') {
        j += 1;
        if bytes.get(j) == Some(&b'x') || bytes.get(j) == Some(&b'X') {
            j += 1;
            let start = j;
            while j < to && bytes[j].is_ascii_hexdigit() && j - start <= 6 {
                j += 1;
            }
        } else {
            let start = j;
            while j < to && bytes[j].is_ascii_digit() && j - start <= 7 {
                j += 1;
            }
        }
    } else {
        let start = j;
        if !bytes.get(j).is_some_and(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        j += 1;
        while j < to && bytes[j].is_ascii_alphanumeric() && j - start <= 31 {
            j += 1;
        }
    }
    (bytes.get(j) == Some(&b';') && j > i + 1).then(|| j + 1)
}

/// 链接/图片：`[label](url)`（balance 找 `]` 与 `)`；span 由 parse_bracket_link 语义判定）。
fn scan_link(ctx: &Ctx, bracket: usize, to: usize, image: bool) -> Option<(usize, InlineSpan)> {
    let src = ctx.source;
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut i = bracket;
    let label_end;
    loop {
        if i >= to {
            return None;
        }
        match bytes[i] {
            b'\\' => {
                i += 1;
                if i >= to {
                    return None;
                }
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    label_end = i;
                    break;
                }
            }
            b'`' => {
                // 行内代码内的括号不参与配对
                let run = run_len(bytes, i, to, b'`');
                if let Some(close) = close_backtick_run(bytes, i + run, to, run) {
                    i = close + run - 1;
                }
            }
            _ => {}
        }
        i += char_len_at(bytes, i);
    }
    if bytes.get(label_end + 1) != Some(&b'(') {
        return None;
    }
    // URL 括号 balance
    let mut j = label_end + 2;
    let mut paren = 0i32;
    let paren_end;
    loop {
        if j >= to {
            return None;
        }
        match bytes[j] {
            b'\\' => {
                j += 1;
                if j >= to {
                    return None;
                }
            }
            b'(' => paren += 1,
            b')' => {
                paren -= 1;
                if paren < 0 {
                    paren_end = j;
                    break;
                }
            }
            _ => {}
        }
        j += char_len_at(bytes, j);
    }
    let whole = &src[bracket..=paren_end];
    let parsed = parse_bracket_link(whole)?;
    let from = bracket;
    let to_b = paren_end + 1;
    let span = if image {
        let mut span = image_span(&parsed)?;
        set_span_range(&mut span, from, to_b);
        span
    } else {
        let form = classify_link(&parsed.label, &parsed.url, ctx.options);
        InlineSpan::Link {
            from,
            to: to_b,
            href: parsed.url.clone(),
            label: if parsed.label.is_empty() { parsed.url } else { parsed.label },
            form,
            title: parsed.title,
        }
    };
    Some((to_b, span))
}

fn set_span_range(span: &mut InlineSpan, from: usize, to: usize) {
    match span {
        InlineSpan::Text { from: f, to: t, .. }
        | InlineSpan::Code { from: f, to: t, .. }
        | InlineSpan::MathInline { from: f, to: t, .. }
        | InlineSpan::Comment { from: f, to: t }
        | InlineSpan::Autolink { from: f, to: t, .. }
        | InlineSpan::Wiki { from: f, to: t, .. }
        | InlineSpan::Tag { from: f, to: t, .. }
        | InlineSpan::FootnoteRef { from: f, to: t, .. }
        | InlineSpan::Html { from: f, to: t, .. }
        | InlineSpan::HardBreak { from: f, to: t }
        | InlineSpan::Link { from: f, to: t, .. }
        | InlineSpan::Image { from: f, to: t, .. }
        | InlineSpan::Mention { from: f, to: t, .. }
        | InlineSpan::Strong { from: f, to: t, .. }
        | InlineSpan::Em { from: f, to: t, .. }
        | InlineSpan::Strike { from: f, to: t, .. }
        | InlineSpan::Highlight { from: f, to: t, .. } => {
            *f = from;
            *t = to;
        }
    }
}

struct BracketLink {
    label: String,
    url: String,
    title: Option<String>,
}

/// `[text](url)` / `![alt](url)`（含可选 title）解析；不匹配返回 None（保持原文）。
fn parse_bracket_link(text: &str) -> Option<BracketLink> {
    // 正则形态：label 无 ]；URL 无空白（形态一，可选 title）或无 ) （形态二）
    if !text.starts_with('[') {
        return None;
    }
    let close = text.find(']')?;
    let label = &text[1..close];
    if !text[close + 1..].starts_with('(') {
        return None;
    }
    let inner = &text[close + 2..];
    let inner = inner.strip_suffix(')')?;
    let url_end = inner
        .char_indices()
        .find(|(_, c)| *c == ')' || c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(inner.len());
    let url_part = &inner[..url_end];
    let after = &inner[url_end..];
    if after.is_empty() {
        return Some(BracketLink { label: label.to_string(), url: url_part.to_string(), title: None });
    }
    // title 形态：URL 后空白 + 引号包裹的 title
    if !after.starts_with(' ') {
        return None;
    }
    let title_part = after.trim_start();
    if title_part.len() >= 2 {
        let q = title_part.chars().next().unwrap();
        if matches!(q, '\'' | '"' | '`') && title_part.ends_with(q) {
            return Some(BracketLink {
                label: label.to_string(),
                url: url_part.to_string(),
                title: Some(title_part[1..title_part.len() - 1].to_string()),
            });
        }
    }
    None
}

fn image_span(parsed: &BracketLink) -> Option<InlineSpan> {
    if parsed.url.is_empty() {
        return None;
    }
    let mut alt = parsed.label.clone();
    let mut width: Option<String> = None;
    let mut height: Option<String> = None;
    if let Some(sep) = alt.rfind('|') {
        if sep > 0 {
            let size = alt[sep + 1..].trim().to_string();
            if is_size_spec(&size) {
                alt.truncate(sep);
                let mut parts = size.splitn(2, 'x');
                width = parts.next().map(|s| s.to_string());
                height = parts.next().map(|s| s.to_string());
            }
        }
    }
    Some(InlineSpan::Image {
        from: 0,
        to: 0,
        src: parsed.url.clone(),
        alt,
        width,
        height,
        title: parsed.title.clone(),
    })
}

fn is_size_spec(s: &str) -> bool {
    let mut parts = s.split('x');
    let w = parts.next().unwrap_or("");
    let h = parts.next();
    if parts.next().is_some() {
        return false;
    }
    let ok = |t: &str| !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit());
    ok(w) && h.is_none_or(ok)
}

/// 链接形态：create（空 URL）/ external（内置判定）/ 其余交宿主判定。
fn classify_link(label: &str, url: &str, options: &ParseOptions) -> LinkForm {
    if url.is_empty() && !label.is_empty() {
        return LinkForm::Create;
    }
    if is_openable_url(url) {
        return LinkForm::External;
    }
    match &options.resolve_link {
        Some(f) => f(url),
        None => LinkForm::Plain,
    }
}

/// 外部链接协议判定（`http:`/`https:`/`mailto:`/`xmpp:` 前缀）。
fn is_openable_url(raw: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^(?:https?:|mailto:|xmpp:)").unwrap());
    // 与 `new URL(raw).protocol` 等价：URL 解析成功且 scheme 命中
    match parse_scheme(raw) {
        Some(scheme) => re.is_match(&format!("{scheme}:")),
        None => false,
    }
}

/// 近似 `new URL` 的 scheme 提取：`scheme:` 前缀（scheme 合法字符集）。
fn parse_scheme(raw: &str) -> Option<String> {
    let idx = raw.find(':')?;
    let scheme = &raw[..idx];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
        return None;
    }
    if scheme.len() < 2 {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

// ===== 强调 =====

struct Delim {
    ch: u8,
    start: usize,
    end: usize,
    count: usize,
    can_open: bool,
    can_close: bool,
}

/// 左右 flanking 判定（CommonMark；`_` 另有词内限制）。
fn flanking(before: Option<char>, after: Option<char>, ch: u8) -> (bool, bool) {
    let ws_before = before.is_none_or(|c| c.is_whitespace());
    let ws_after = after.is_none_or(|c| c.is_whitespace());
    let p_before = before.is_some_and(is_punct_char);
    let p_after = after.is_some_and(is_punct_char);
    let left = !ws_after && (!p_after || ws_before || p_before);
    let right = !ws_before && (!p_before || ws_after || p_after);
    if ch == b'_' {
        let open = left && (!right || p_before);
        let close = right && (!left || p_after);
        (open, close)
    } else {
        (left, right)
    }
}

fn is_punct_char(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_ascii())
}

fn process_emphasis(ctx: &Ctx, delims: &mut Vec<Delim>, from: usize, to: usize) -> Vec<Atom> {
    let _ = (from, to);
    let mut containers: Vec<Atom> = Vec::new();
    let mut closer_idx = 0usize;
    // 同一对 run 的多轮配对：inner 直接复用上一轮产物（如 ***x*** 强调套强调）
    let mut last_node: Option<Atom> = None;
    let mut last_pair: Option<(usize, usize)> = None;
    while closer_idx < delims.len() {
        if !delims[closer_idx].can_close || delims[closer_idx].count == 0 {
            closer_idx += 1;
            continue;
        }
        let mut opener_idx: Option<usize> = None;
        for oi in (0..closer_idx).rev() {
            let o = &delims[oi];
            let c = &delims[closer_idx];
            if o.ch == c.ch && o.can_open && o.count > 0 {
                opener_idx = Some(oi);
                break;
            }
        }
        let Some(oi) = opener_idx else {
            closer_idx += 1;
            continue;
        };
        let ch = delims[oi].ch;
        let strong = ch != b'~' && delims[oi].count >= 2 && delims[closer_idx].count >= 2;
        let used = if ch == b'~' {
            // GFM 删除线：恰两个 ~ 配对（单个 ~ 为字面文本）
            if delims[oi].count == 2 && delims[closer_idx].count == 2 {
                2
            } else {
                closer_idx += 1;
                continue;
            }
        } else if strong {
            2
        } else {
            1
        };
        // 节点区间：used 个 delimiter 从 run 内侧取
        let node_from = delims[oi].end - used;
        let node_to = delims[closer_idx].start + used;
        let reuse = last_pair == Some((oi, closer_idx));
        let children: Vec<InlineSpan> = if reuse {
            match last_node.take() {
                Some(prev) => vec![prev.span],
                None => parse_inline_range(ctx, delims[oi].end, delims[closer_idx].start),
            }
        } else {
            parse_inline_range(ctx, delims[oi].end, delims[closer_idx].start)
        };
        let span = match (ch, strong) {
            (b'~', _) => InlineSpan::Strike { from: node_from, to: node_to, children },
            (_, true) => InlineSpan::Strong { from: node_from, to: node_to, children },
            _ => InlineSpan::Em { from: node_from, to: node_to, children },
        };
        containers.push(Atom { from: node_from, to: node_to, span });
        if reuse {
            last_node = None;
            last_pair = None;
        } else {
            last_node = containers.last().cloned();
            last_pair = Some((oi, closer_idx));
        }
        // 消耗 delimiter（内侧取用：opener 的 end 内缩、closer 的 start 内缩）
        delims[oi].end -= used;
        delims[oi].count -= used;
        delims[closer_idx].start += used;
        delims[closer_idx].count -= used;
        if delims[oi].count == 0 {
            delims.remove(oi);
            if oi < closer_idx {
                closer_idx -= 1;
                if delims[closer_idx].count == 0 {
                    closer_idx += 1;
                    last_node = None;
                    last_pair = None;
                }
            }
        } else if delims[closer_idx].count == 0 {
            closer_idx += 1;
            last_node = None;
            last_pair = None;
        }
        // 双方均有余量：同一对继续配对（closer_idx 不变）
    }
    containers
}

// ===== 按行正则补充 =====

fn scan_line_matches(ctx: &Ctx, from: usize, to: usize) -> Vec<Atom> {
    let src = ctx.source;
    let mut out: Vec<Atom> = Vec::new();
    let mut line_start = src[..from].rfind('\n').map(|i| i + 1).unwrap_or(0);
    while line_start < to {
        let nl = src[line_start..to].find('\n').map(|i| line_start + i).unwrap_or(to);
        let line_text = &src[line_start..nl];
        scan_one_line(ctx, line_text, line_start, &mut out);
        line_start = nl + 1;
    }
    out
}

fn match_all<'a>(re: &Regex, text: &'a str) -> Vec<regex::Captures<'a>> {
    re.captures_iter(text).collect()
}

fn scan_one_line(ctx: &Ctx, line_text: &str, line_from: usize, out: &mut Vec<Atom>) {
    let push =
        |from: usize, to: usize, span: InlineSpan, out: &mut Vec<Atom>| out.push(Atom { from, to, span });

    static RE_COMMENT: OnceLock<Regex> = OnceLock::new();
    let re_comment = RE_COMMENT.get_or_init(|| Regex::new(r"%%([^%\n]+)%%").unwrap());
    for m in match_all(re_comment, line_text) {
        let from = line_from + m.get(0).unwrap().start();
        let to = from + m.get(0).unwrap().len();
        push(from, to, InlineSpan::Comment { from, to }, out);
    }

    static RE_HL: OnceLock<Regex> = OnceLock::new();
    let re_hl = RE_HL.get_or_init(|| Regex::new(r"==([^=\n]+)==").unwrap());
    for m in match_all(re_hl, line_text) {
        let from = line_from + m.get(0).unwrap().start();
        let to = from + m.get(0).unwrap().len();
        let children = parse_inline_range(ctx, from + 2, to - 2);
        push(from, to, InlineSpan::Highlight { from, to, children }, out);
    }

    static RE_WIKI: OnceLock<Regex> = OnceLock::new();
    let re_wiki = RE_WIKI.get_or_init(|| Regex::new(r"\[\[([^\]|]*?)(?:\|([^\]]*?))?\]\]").unwrap());
    for m in match_all(re_wiki, line_text) {
        let target = m.get(1).map(|g| g.as_str().trim().to_string()).unwrap_or_default();
        if target.is_empty() {
            continue;
        }
        let from = line_from + m.get(0).unwrap().start();
        let to = from + m.get(0).unwrap().len();
        let label = m
            .get(2)
            .map(|g| g.as_str().trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| target.clone());
        push(from, to, InlineSpan::Wiki { from, to, target, label }, out);
    }

    static RE_FN: OnceLock<Regex> = OnceLock::new();
    let re_fn = RE_FN.get_or_init(|| Regex::new(r"\[\^([^\]]+)\]").unwrap());
    for m in match_all(re_fn, line_text) {
        let label = m.get(1).map(|g| g.as_str().to_string()).unwrap_or_default();
        if label.is_empty() {
            continue;
        }
        let from = line_from + m.get(0).unwrap().start();
        let to = from + m.get(0).unwrap().len();
        push(from, to, InlineSpan::FootnoteRef { from, to, label }, out);
    }

    for r in inline_math_ranges(line_text, line_from) {
        push(
            r.from,
            r.to,
            InlineSpan::MathInline {
                from: r.from,
                to: r.to,
                tex: ctx.source[r.from + 1..r.to - 1].to_string(),
            },
            out,
        );
    }

    // mention 先于 tag：宿主候选优先于标签语法，mention 命中区间不再产出 tag
    if ctx.options.mentions.is_empty() {
        for r in inline_tag_ranges(line_text, line_from) {
            push(
                r.from,
                r.to,
                InlineSpan::Tag { from: r.from, to: r.to, tag: ctx.source[r.from + 1..r.to].to_string() },
                out,
            );
        }
    } else {
        let mut mention_ranges: Vec<RangeInfo> = Vec::new();
        for trigger in ['@', '#'] {
            let mut i = 0usize;
            while i < line_text.len() {
                let at = match line_text[i..].find(trigger) {
                    Some(off) => i + off,
                    None => break,
                };
                let prev_ok = at == 0 || {
                    let prev = prev_char(line_text, at);
                    prev.is_none_or(|c| !is_word_char(c))
                };
                if !prev_ok {
                    i = at + 1;
                    continue;
                }
                let mut best: Option<&MentionCandidate> = None;
                for mn in &ctx.options.mentions {
                    if mn.label.is_empty() {
                        continue;
                    }
                    if !line_text[at + 1..].starts_with(&mn.label) {
                        continue;
                    }
                    let after = line_text[at + 1 + mn.label.len()..].chars().next();
                    let label_ends_word = mn.label.chars().next_back().is_some_and(is_word_char);
                    if after.is_none() || !label_ends_word || !after.is_some_and(is_word_char) {
                            best = best.filter(|b| mn.label.len() > b.label.len()).or(Some(mn));
                        }
                }
                let Some(mn) = best else {
                    i = at + 1;
                    continue;
                };
                let from = line_from + at;
                let to = from + 1 + mn.label.len();
                push(
                    from,
                    to,
                    InlineSpan::Mention {
                        from,
                        to,
                        key: mn.key.clone(),
                        label: mn.label.clone(),
                        trigger,
                    },
                    out,
                );
                mention_ranges.push(RangeInfo { from, to });
                i = to - line_from;
            }
        }
        for r in inline_tag_ranges(line_text, line_from) {
            if mention_ranges.iter().any(|m| r.from < m.to && r.to > m.from) {
                continue;
            }
            push(
                r.from,
                r.to,
                InlineSpan::Tag { from: r.from, to: r.to, tag: ctx.source[r.from + 1..r.to].to_string() },
                out,
            );
        }
    }
}

fn prev_char(text: &str, byte_pos: usize) -> Option<char> {
    text[..byte_pos].chars().next_back()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// 行内 `#标签` 区间（`#` 前允许非词字符，词内 `#` 不误判）。
fn inline_tag_ranges(line_text: &str, line_from: usize) -> Vec<RangeInfo> {
    static RE_TAG: OnceLock<Regex> = OnceLock::new();
    let re = RE_TAG.get_or_init(|| Regex::new(r"(?:^|[^\p{L}\p{N}_#/])#([\p{L}\p{N}_\-/]+)").unwrap());
    let mut out = Vec::new();
    for m in match_all(re, line_text) {
        let tag = m.get(1).map(|g| g.as_str()).unwrap_or("");
        if !tag.chars().any(|c| c.is_alphabetic()) {
            continue;
        }
        let lead = m.get(0).unwrap().len() - 1 - tag.len();
        let start = line_from + m.get(0).unwrap().start() + lead;
        out.push(RangeInfo { from: start, to: start + 1 + tag.len() });
    }
    out
}

/// 行内数学 `$...$`（单 `$` 定界、内容不跨行、非 `$$`、内容不以空格起止）。
fn inline_math_ranges(line_text: &str, line_from: usize) -> Vec<RangeInfo> {
    let bytes = line_text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(idx_rel) = line_text[i..].find('$') else { break };
        let idx = i + idx_rel;
        i = idx + 1;
        if idx > 0 && bytes[idx - 1] == b'\\' {
            continue;
        }
        if bytes.get(idx + 1) == Some(&b'$') {
            continue;
        }
        let Some(close_rel) = line_text[idx + 1..].find('$') else { break };
        let close = idx + 1 + close_rel;
        if bytes.get(close + 1) == Some(&b'$') {
            i = close + 1;
            continue;
        }
        let content = &line_text[idx + 1..close];
        let trimmed = content.trim();
        if !trimmed.is_empty() && !content.starts_with(' ') && !content.ends_with(' ') {
            out.push(RangeInfo { from: line_from + idx, to: line_from + close + 1 });
        }
        i = close + 1;
    }
    out
}

fn is_ascii_punct(b: u8) -> bool {
    b.is_ascii_punctuation()
}
