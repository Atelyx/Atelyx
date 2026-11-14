//! 笔记内部链接的跨度扫描与改写：反链提取与链接重建/维护共用同一套跨度判定。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::index::WikiRef;
use super::{percent_decode, safe_join, sanitize_filename, sync_dir, walk_md_in, write_tmp};

/// 若 `i` 处位于行首（i == 0 或前一字符为换行）。
pub(super) fn at_line_start(content: &str, i: usize) -> bool {
    i == 0 || content.as_bytes()[i - 1] == b'\n'
}

/// `i` 处若为围栏行首（``` 或 ~~~，≥3 个同字符），返回围栏长度。
pub(super) fn fence_len_at(content: &str, i: usize) -> Option<usize> {
    let ch = content[i..].chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let n = content[i..].chars().take_while(|&c| c == ch).count();
    if n >= 3 {
        Some(n)
    } else {
        None
    }
}

/// 在 `rest`（围栏开启后的剩余内容）中找行首同字符闭合围栏（≥ open_len，前导空格 ≤3），
/// 返回闭合围栏所在行的结束字节位置（含行尾换行）。
pub(super) fn fence_close_end(rest: &str, open_char: char, open_len: usize) -> Option<usize> {
    let mut pos = 0usize;
    for line in rest.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let lead = trimmed.chars().take_while(|&c| c == ' ').count();
        if lead <= 3 {
            let body = &trimmed[lead..];
            let n = body.chars().take_while(|&c| c == open_char).count();
            if n >= open_len {
                return Some(start + line.len());
            }
        }
    }
    None
}

/// 反引号 run 长度（i 处起连续反引号数）。
pub(super) fn backtick_run_at(content: &str, i: usize) -> usize {
    content[i..].bytes().take_while(|&b| b == b'`').count()
}

/// 从 `from` 起找 ≥n 个连续反引号的闭合位置（run 之后）；无闭合返回 None。
pub(super) fn backtick_close(content: &str, from: usize, n: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run = backtick_run_at(content, i);
            if run >= n {
                return Some(i + run);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// 链接跨度种类：反链提取与改写共用同一套判定，差别只在「哪些种类算引用 / 哪些可改写」。
#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkSpanKind {
    /// `[[target]]` / `[[target|别名]]`
    Wiki,
    /// `[label](path)`
    Markdown,
    /// `![[target]]`：渲染成可见的 `!` + 链接，计入反链但不按链接改写。
    Embed,
}

/// 链接跨度扫描：跳过 frontmatter/围栏代码/缩进代码/原始 HTML/行内代码，对每个链接跨度调用
/// `visit`（返回 None = 原样保留，Some = 换成该文本）。反链提取与改写都走这一份判定，
/// 否则「反链命中但改写漏」或反之（同一个链接两处结论不同）。
fn scan_link_spans(content: &str, visit: &mut dyn FnMut(&str, LinkSpanKind) -> Option<String>) -> String {
    let mut out = String::with_capacity(content.len());
    let len = content.len();
    let mut i = 0;
    // 文档开头 frontmatter：整体跳过（YAML 里出现 `[[`/`](` 是数据不是链接）
    if content.starts_with("---") {
        if let Some(end) = crate::frontmatter::block_end(content) {
            out.push_str(&content[..end]);
            i = end;
        }
    }
    while i < len {
        // 块级跳过（CommonMark 语义，防链接语法在代码/HTML 中被改写）：
        // - 缩进代码块（≥4 空格缩进行）；
        // - 原始 HTML：行首 `<`（标签块以空行结束；`<!--` 注释可跨行至 `-->`）
        if at_line_start(content, i) {
            let rest = &content[i..];
            let lead = rest.bytes().take_while(|&b| b == b' ').count();
            if lead >= 4 {
                let end = rest.find('\n').map_or(len, |p| i + p + 1);
                out.push_str(&content[i..end]);
                i = end;
                continue;
            }
            let after = &rest[lead..];
            let html_start = after.starts_with("<!--")
                || after.starts_with("<?")
                || after.starts_with("<!")
                || after.starts_with("</")
                || (after.starts_with('<')
                    && after.len() > 1
                    && after.as_bytes()[1].is_ascii_alphabetic());
            if html_start {
                if after.starts_with("<!--") {
                    // HTML 注释：可跨行，跳至 `-->`（未闭合则余下整体跳过）
                    let from = i + lead + 4;
                    let end = content[from..].find("-->").map_or(len, |p| from + p + 3);
                    out.push_str(&content[i..end]);
                    i = end;
                } else {
                    // 标签块：以空行结束，逐行跳过
                    let mut cursor = i;
                    loop {
                        match content[cursor..].find('\n') {
                            Some(p) => {
                                let line_end = cursor + p;
                                if content[cursor..line_end].trim().is_empty() {
                                    cursor = line_end;
                                    break;
                                }
                                cursor = line_end + 1;
                            }
                            None => {
                                cursor = len;
                                break;
                            }
                        }
                    }
                    out.push_str(&content[i..cursor]);
                    i = cursor;
                }
                continue;
            }
        }
        // 围栏代码块：行首 ``` / ~~~，跳至行首同字符闭合围栏
        if at_line_start(content, i) {
            if let Some(open_len) = fence_len_at(content, i) {
                let open_char = content[i..].chars().next().unwrap();
                if let Some(close) = fence_close_end(&content[i + open_len..], open_char, open_len) {
                    let end = i + open_len + close;
                    out.push_str(&content[i..end]);
                    i = end;
                    continue;
                }
            }
        }
        // 行内代码：反引号 run，跳至等长（或更长）run 闭合
        if content.as_bytes()[i] == b'`' {
            let n = backtick_run_at(content, i);
            if let Some(close) = backtick_close(content, i + n, n) {
                out.push_str(&content[i..close]);
                i = close;
                continue;
            }
        }
        // wiki 链接 `[[..]]`；前导 `!` 是嵌入语法（`![[a]]`），计入反链但不改写——
        // 改写成 `[..](..)` 会丢掉 `!` 变成图片语法
        if content[i..].starts_with("[[") {
            if let Some(end_rel) = content[i + 2..].find("]]") {
                let end = i + 2 + end_rel + 2;
                let kind = if i > 0 && content.as_bytes()[i - 1] == b'!' {
                    LinkSpanKind::Embed
                } else {
                    LinkSpanKind::Wiki
                };
                match visit(&content[i..end], kind) {
                    Some(replacement) => out.push_str(&replacement),
                    None => out.push_str(&content[i..end]),
                }
                i = end;
                continue;
            }
        }
        // `[label](path)`：前导非 `!`（图片链接不处理）
        if content.as_bytes()[i] == b'[' && (i == 0 || content.as_bytes()[i - 1] != b'!') {
            if let Some(rel) = content[i + 1..].find(']') {
                let after_label = i + 1 + rel + 1;
                if content.as_bytes().get(after_label) == Some(&b'(') {
                    if let Some(rel2) = content[after_label + 1..].find(')') {
                        let end = after_label + 1 + rel2 + 1;
                        match visit(&content[i..end], LinkSpanKind::Markdown) {
                            Some(replacement) => out.push_str(&replacement),
                            None => out.push_str(&content[i..end]),
                        }
                        i = end;
                        continue;
                    }
                }
            }
        }
        let ch_len = content[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&content[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// 链接跨度改写引擎：跳过 frontmatter/围栏代码/缩进代码/原始 HTML/行内代码/图片链接，
/// 对每个可改写的链接跨度调用 `apply`（返回原样 = 不改）。仅用于字节级替换（重建/维护），不用于解析。
pub fn rewrite_link_spans(content: &str, apply: &mut dyn FnMut(&str) -> String) -> String {
    scan_link_spans(content, &mut |span, kind| match kind {
        LinkSpanKind::Embed => None,
        _ => Some(apply(span)),
    })
}

/// 反链提取：与改写共用同一条扫描（代码块 / frontmatter / 行内代码 / 缩进代码 / 原始 HTML 里的
/// 链接都不算），只把链接收成引用、不改文本。嵌入 `![[..]]` 计入，图片 `![..](..)` 不计入。
/// 名字引用排在路径引用之前（沿用拆分前的提取顺序）。
pub(super) fn extract_refs(content: &str) -> Vec<WikiRef> {
    let mut named: Vec<WikiRef> = Vec::new();
    let mut paths: Vec<WikiRef> = Vec::new();
    scan_link_spans(content, &mut |span, kind| {
        match kind {
            LinkSpanKind::Wiki | LinkSpanKind::Embed => {
                let target = &span[2..span.len() - 2];
                let name = target.split('|').next().unwrap_or(target).trim();
                if !name.is_empty() {
                    named.push(WikiRef {
                        name: Some(name.to_string()),
                        path: None,
                    });
                }
            }
            LinkSpanKind::Markdown => {
                if let Some(path) = markdown_link_path(span) {
                    if let Some(normalized) = normalize_link_path(path) {
                        paths.push(WikiRef {
                            name: None,
                            path: Some(normalized),
                        });
                    }
                }
            }
        }
        None
    });
    named.extend(paths);
    named
}

/// 归一化链接路径：percent 解码、反斜杠转 `/`、退掉前导分隔符（跨平台链接形如 `/notes/a.md`），
/// 再交给 [crate::path::normalize] 统一口径；含 `..` 段或空结果返回 None（防越出仓库）。
fn normalize_link_path(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw).replace('\\', "/");
    let rel = decoded.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    crate::path::normalize(rel).ok()
}

/// 取标准链接跨度 `[label](path)` 的 path 部分（wiki 形式 `[[..]]` 返回 None）。
pub fn markdown_link_path(span: &str) -> Option<&str> {
    if span.starts_with("[[") {
        return None;
    }
    let open = span.find("](")?;
    Some(&span[open + 2..span.len() - 1])
}

/// 收集需更新 markdown 内部链接的笔记（不写盘；事务模式与 collect_canvas_updates 同构）。
/// `apply`：对每个链接跨度返回替换结果（原样 = 不改）。返回 `(相对路径, 新内容)` 列表。
pub fn collect_md_link_updates(
    root: &Path,
    exclude: &[String],
    apply: &mut dyn FnMut(&str) -> String,
) -> Result<Vec<(String, String)>, String> {
    let mut updates: Vec<(String, String)> = vec![];
    walk_md_in(root, "", exclude, &mut |rel, path| {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Ok(()), // 不可读跳过，不阻塞其余
        };
        let next = rewrite_link_spans(&content, apply);
        if next != content {
            updates.push((rel.to_string(), next));
        }
        Ok(())
    })?;
    Ok(updates)
}

/// 写回收集的 .md 更新：两阶段提交——先把全部内容落成各自 tmp，全部成功后再逐个 rename。
///
/// 写入阶段失败（磁盘满/权限/路径非法）即清理全部 tmp 返回 Err，磁盘保持**全部旧内容**；
/// 逐个 rename 半途失败时同样清理剩余 tmp（已 rename 的无法回退）。跨多文件的事务在文件系统层
/// 无法做到真正原子，本函数消除的是「内容写入期失败留下部分文件已改」这一主要半写来源。
pub fn flush_md_updates(root: &Path, updates: &[(String, String)]) -> Result<(), String> {
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(updates.len());
    let discard = |staged: &[(PathBuf, PathBuf)]| {
        for (tmp, _) in staged {
            let _ = std::fs::remove_file(tmp);
        }
    };

    // 阶段一：内容全部落 tmp（不触碰目标文件）
    for (rel, content) in updates {
        let target = match safe_join(root, rel, true) {
            Ok(p) => p,
            Err(e) => {
                discard(&staged);
                return Err(e);
            }
        };
        match write_tmp(&target, content.as_bytes()) {
            Ok(tmp) => staged.push((tmp, target)),
            Err(e) => {
                discard(&staged);
                return Err(e);
            }
        }
    }

    // 阶段二：逐个提交（同目录 rename，失败窗口远小于写入期）
    for (i, (tmp, target)) in staged.iter().enumerate() {
        if let Err(e) = std::fs::rename(tmp, target) {
            let _ = std::fs::remove_file(tmp);
            discard(&staged[i + 1..]);
            return Err(format!("写入失败：{e}"));
        }
    }

    // 与 atomic_write 同一下 durability 语义：提交后 fsync 各目标父目录（去重，避免同目录重复 fsync）
    let mut dirs: HashSet<&Path> = HashSet::new();
    for (_, target) in &staged {
        if let Some(parent) = target.parent() {
            dirs.insert(parent);
        }
    }
    for dir in dirs {
        sync_dir(dir);
    }
    Ok(())
}

/// 解析内部链接目标（笔记名或相对路径）→ 规范相对路径。
/// - 候选构造：`name` 已是 `.md` 路径（`[x](路径.md)` 形式）→ 原样作候选；否则按笔记名补 `.md` + 净化变体；
/// - 匹配顺序：精确路径 → 文件名命中 → **同名歧义取「路径最短（根目录优先）+ 字典序」确定性兜底**
///   （首个命中的可打开语义，防同名链接失效）；
/// - 全部分支后做大小写不敏感兜底：Windows 文件系统不区分大小写，`方案.MD` 与 `方案.md` 是同一文件。
pub fn resolve_link_target(
    name: &str,
    exact: &HashSet<String>,
    by_basename: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let mut candidates: Vec<String> = if name.to_ascii_lowercase().ends_with(".md") {
        vec![name.to_string()]
    } else {
        vec![format!("{name}.md")]
    };
    if !name.to_ascii_lowercase().ends_with(".md") {
        let sanitized = sanitize_filename(name);
        if !sanitized.is_empty() && sanitized != name {
            candidates.push(format!("{sanitized}.md"));
        }
    }
    let pick = |rels: &Vec<String>| -> Option<String> {
        rels.iter()
            .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
            .cloned()
    };
    for cand in &candidates {
        if exact.contains(cand) {
            return Some(cand.clone());
        }
        if let Some(rels) = by_basename.get(cand) {
            if let Some(best) = pick(rels) {
                return Some(best);
            }
        }
    }
    for cand in &candidates {
        if let Some(hit) = exact.iter().find(|s| s.eq_ignore_ascii_case(cand)) {
            return Some(hit.clone());
        }
        if let Some((_, rels)) = by_basename.iter().find(|(k, _)| k.eq_ignore_ascii_case(cand)) {
            if let Some(best) = pick(rels) {
                return Some(best);
            }
        }
    }
    None
}

/// 一次性重建内部链接（设置 → 编辑器「重建内部链接」入口）：
/// - `[[名]]` / `[[名|别名]]` → `[名](规范相对路径)` / `[别名](规范相对路径)`；
/// - `[label](路径.md)` 命中仓库内 .md → 规范为 `[label](规范相对路径)`（解码 + 归一化）；
/// - `[名]()` 空路径 → 按 label 当笔记名解析，命中则补全为规范路径，仍不存在则保持空路径；
/// - 目标笔记不存在 → `[名]()`（空路径，点击可快捷新建）；
/// - 外部链接 / 非 .md 相对路径 / 图片链接 / 代码块内 → 一律不动（保守不猜）。
///
/// `resolve`：笔记名或路径 → 命中时的规范相对路径（None = 未命中）。返回 (新内容, 实际改写处数)。
pub fn rewrite_internal_links(
    content: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> (String, usize) {
    let mut count = 0usize;
    let out = rewrite_link_spans(content, &mut |span: &str| {
        let is_wiki = span.starts_with("[[");
        let (label, target): (&str, Option<String>) = if is_wiki {
            let inner = &span[2..span.len() - 2];
            let (name, alias) = match inner.split_once('|') {
                Some((n, a)) => (n.trim(), a.trim()),
                None => (inner.trim(), ""),
            };
            let label = if alias.is_empty() { name } else { alias };
            (label, resolve(name))
        } else {
            let Some(path) = markdown_link_path(span) else {
                return span.to_string();
            };
            let label = &span[1..span.find("](").unwrap_or(1)];
            if path.is_empty() {
                // `[名]()`：按 label 当笔记名解析，命中则补全路径；仍不存在则保持空路径（点击快捷新建）
                if let Some(target) = resolve(label) {
                    count += 1;
                    return format!("[{label}]({target})");
                }
                return span.to_string();
            }
            let norm = normalize_link_path(path);
            if norm.is_none() {
                return span.to_string();
            }
            let resolved = resolve(norm.as_deref().unwrap());
            // 未命中但仍是 .md 形状（大小写不敏感，Windows 文件系统语义）→ 空路径（可快捷新建）；非 .md（附件等）不动
            if resolved.is_none() && !norm.as_deref().unwrap().to_ascii_lowercase().ends_with(".md")
            {
                return span.to_string();
            }
            (label, resolved)
        };
        let replaced: String = match target {
            Some(p) => format!("[{label}]({p})"),
            None => format!("[{label}]()"),
        };
        if replaced != span {
            count += 1;
        }
        replaced
    });
    (out, count)
}

#[cfg(test)]
mod link_rewrite_tests {
    use super::*;

    /// 按命令层同款逻辑构造解析器（exact + by_basename → resolve_link_target）。
    fn resolver<'a>(files: &'a [&'a str]) -> impl Fn(&str) -> Option<String> + 'a {
        let exact: HashSet<String> = files.iter().map(|s| s.to_string()).collect();
        let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();
        for rel in files {
            let base = rel.rsplit('/').next().unwrap_or(rel).to_string();
            by_basename.entry(base).or_default().push(rel.to_string());
        }
        move |name: &str| resolve_link_target(name, &exact, &by_basename)
    }

    #[test]
    fn wiki_link_root_note() {
        let resolve = resolver(&["note-a.md"]);
        let (out, n) = rewrite_internal_links("见 [[note-a]]", &resolve);
        assert_eq!(out, "见 [note-a](note-a.md)");
        assert_eq!(n, 1);
    }

    #[test]
    fn wiki_link_subfolder_unique() {
        let resolve = resolver(&["notes/note-b.md"]);
        let (out, n) = rewrite_internal_links("见 [[note-b]]", &resolve);
        assert_eq!(out, "见 [note-b](notes/note-b.md)");
        assert_eq!(n, 1);
    }

    #[test]
    fn wiki_link_alias_form() {
        let resolve = resolver(&["notes/note-b.md"]);
        let (out, n) = rewrite_internal_links("见 [[note-b|alias-b]]", &resolve);
        assert_eq!(out, "见 [alias-b](notes/note-b.md)");
        assert_eq!(n, 1);
    }

    #[test]
    fn path_link_exact_and_basename() {
        let resolve = resolver(&["notes/note-b.md"]);
        // 完整路径链接：已是规范形式，不做追加 .md 的候选处理，保持原样
        let (out, n) = rewrite_internal_links("[note-b](notes/note-b.md)", &resolve);
        assert_eq!(out, "[note-b](notes/note-b.md)");
        assert_eq!(n, 0);
        // 文件名形式链接：按 basename 命中并规范为完整路径
        let (out2, n2) = rewrite_internal_links("[note-b](note-b.md)", &resolve);
        assert_eq!(out2, "[note-b](notes/note-b.md)");
        assert_eq!(n2, 1);
    }

    #[test]
    fn path_link_encoded_normalized() {
        let resolve = resolver(&["topic a.md"]);
        let (out, n) = rewrite_internal_links("[doc](topic%20a.md)", &resolve);
        assert_eq!(out, "[doc](topic a.md)");
        assert_eq!(n, 1);
    }

    #[test]
    fn wiki_link_duplicate_basename_keeps_resolvable() {
        // 回归：同名多文件不得转空路径——根目录优先（路径最短），确定性兜底
        let resolve = resolver(&["note-b.md", "notes/note-b.md"]);
        let (out, n) = rewrite_internal_links("见 [[note-b]]", &resolve);
        assert_eq!(out, "见 [note-b](note-b.md)");
        assert_eq!(n, 1);
    }

    #[test]
    fn missing_note_becomes_empty() {
        let resolve = resolver(&[]);
        let (out, n) = rewrite_internal_links("见 [[missing-note]]", &resolve);
        assert_eq!(out, "见 [missing-note]()");
        assert_eq!(n, 1);
    }

    #[test]
    fn empty_link_recovered_by_label() {
        // `[名]()` 空路径链接按 label 解析命中 → 补全为规范路径
        let resolve = resolver(&["notes/note-b.md"]);
        let (out, n) = rewrite_internal_links("见 [note-b]()", &resolve);
        assert_eq!(out, "见 [note-b](notes/note-b.md)");
        assert_eq!(n, 1);
    }

    #[test]
    fn empty_link_kept_when_note_missing() {
        let resolve = resolver(&[]);
        let (out, n) = rewrite_internal_links("见 [note-b]()", &resolve);
        assert_eq!(out, "见 [note-b]()");
        assert_eq!(n, 0);
    }

    #[test]
    fn skips_code_block_inline_code_image_external() {
        let resolve = resolver(&["note-b.md"]);
        let content =
            "```\n[[note-b]]\n```\n\n`[[note-b]]`\n\n![img](note-b.md)\n\n[ext](https://example.com)\n\n[[note-b]]";
        let (out, n) = rewrite_internal_links(content, &resolve);
        assert_eq!(
            out,
            "```\n[[note-b]]\n```\n\n`[[note-b]]`\n\n![img](note-b.md)\n\n[ext](https://example.com)\n\n[note-b](note-b.md)"
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn embed_notation_skipped() {
        // `![[a]]` 是嵌入语法（前导 !），不按链接改写——防转成图片语法 `![a](a.md)`
        let resolve = resolver(&["note-b.md"]);
        let (out, n) = rewrite_internal_links("![[note-b]]", &resolve);
        assert_eq!(out, "![[note-b]]");
        assert_eq!(n, 0);
    }

    #[test]
    fn skips_indented_code_and_raw_html() {
        let resolve = resolver(&["note-b.md"]);
        let content =
            "    [[note-b]]\n    [x](note-b.md)\n\n<div>\n[[note-b]]\n</div>\n\n<!--\n[[note-b]]\n-->\n\n见 [[note-b]]";
        let (out, n) = rewrite_internal_links(content, &resolve);
        assert_eq!(
            out,
            "    [[note-b]]\n    [x](note-b.md)\n\n<div>\n[[note-b]]\n</div>\n\n<!--\n[[note-b]]\n-->\n\n见 [note-b](note-b.md)"
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn case_insensitive_resolution() {
        // Windows 文件系统不区分大小写：`note-b.MD` 与 `note-b.md` 是同一文件
        let resolve = resolver(&["notes/note-b.md"]);
        let (out, n) = rewrite_internal_links("[x](note-b.MD)", &resolve);
        assert_eq!(out, "[x](notes/note-b.md)");
        assert_eq!(n, 1);
        let (out2, n2) = rewrite_internal_links("[[note-b]]", &resolve);
        assert_eq!(out2, "[note-b](notes/note-b.md)");
        assert_eq!(n2, 1);
    }

    #[test]
    fn frontmatter_skipped() {
        let resolve = resolver(&["note-b.md"]);
        let content = "---\ntags: [[note-b]]\n---\n\n见 [[note-b]]";
        let (out, _) = rewrite_internal_links(content, &resolve);
        assert_eq!(out, "---\ntags: [[note-b]]\n---\n\n见 [note-b](note-b.md)");
    }
}

#[cfg(test)]
mod link_scan_tests {
    use super::*;

    /// 把每个被判为链接的跨度替换成 [x]，返回（改写结果, 命中跨度数）。
    fn rewritten(content: &str) -> (String, usize) {
        let mut hits = 0;
        let out = rewrite_link_spans(content, &mut |_span| {
            hits += 1;
            "[x]".to_string()
        });
        (out, hits)
    }

    #[test]
    fn code_block_links_are_neither_backlink_refs_nor_rewritten() {
        let content = "```\n[[note-b]]\n[note-b](note-b.md)\n```\n\n[[note-b]]";
        // 反链提取与改写必须同口径：围栏代码块内不算链接
        assert_eq!(extract_refs(content).len(), 1, "代码块内的链接不得计入反链");
        let (out, hits) = rewritten(content);
        assert_eq!(hits, 1, "代码块内的链接不得被改写");
        assert_eq!(out, "```\n[[note-b]]\n[note-b](note-b.md)\n```\n\n[x]");
    }

    #[test]
    fn frontmatter_links_are_neither_backlink_refs_nor_rewritten() {
        let content = "---\ntags: [[note-b]]\nlink: [note-b](note-b.md)\n---\n\n[[note-b]]";
        assert_eq!(extract_refs(content).len(), 1, "frontmatter 内的链接不得计入反链");
        let (out, hits) = rewritten(content);
        assert_eq!(hits, 1, "frontmatter 内的链接不得被改写");
        assert_eq!(out, "---\ntags: [[note-b]]\nlink: [note-b](note-b.md)\n---\n\n[x]");
    }

    #[test]
    fn inline_and_indented_code_links_are_neither_refs_nor_rewritten() {
        let content = "`[[note-b]]`\n\n    [[note-b]]\n\n[[note-b]]";
        assert_eq!(extract_refs(content).len(), 1, "行内代码与缩进代码内的链接不得计入反链");
        let (out, hits) = rewritten(content);
        assert_eq!(hits, 1);
        assert_eq!(out, "`[[note-b]]`\n\n    [[note-b]]\n\n[x]");
    }

    #[test]
    fn image_is_not_a_ref_but_embed_wiki_link_still_counts() {
        // 图片 ![..](..) 不是反链；![[..]] 渲染为可见的 `!` + 链接，仍计入反链
        assert!(extract_refs("![img](note-b.md)").is_empty());
        assert_eq!(extract_refs("![[note-b]]").len(), 1);
        // 两者都不按链接改写（防把嵌入改成图片语法）
        assert_eq!(rewritten("![img](note-b.md)\n\n![[note-b]]").1, 0);
    }
    #[test]
    fn refs_follow_old_extraction_contract() {
        // `[[a|别名]]` 取竖线前的主名；空路径 markdown 链接不是有效引用（无可归一化路径）
        let refs = extract_refs("[[a|别名]]");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name.as_deref(), Some("a"));
        assert!(extract_refs("[x]()").is_empty(), "空路径链接不计入反链");
    }
}
#[cfg(test)]
mod flush_md_updates_tests {
    use crate::vault::test_support::{tmp_leftovers, TempDir};
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn updates_of(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn commits_all_updates_and_leaves_no_tmp() {
        let root = TempDir::new("flushmd-commit");
        write(&root, "a.md", "A1");
        write(&root, "子目录/b.md", "B1");
        let updates = updates_of(&[("a.md", "A2"), ("子目录/b.md", "B2"), ("新目录/c.md", "C1")]);
        flush_md_updates(&root, &updates).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), "A2");
        assert_eq!(std::fs::read_to_string(root.join("子目录/b.md")).unwrap(), "B2");
        assert_eq!(std::fs::read_to_string(root.join("新目录/c.md")).unwrap(), "C1");
        assert!(tmp_leftovers(&root).is_empty(), "残留：{:?}", tmp_leftovers(&root));
    }

    #[test]
    fn write_failure_keeps_every_target_unchanged() {
        let root = TempDir::new("flushmd-rollback");
        write(&root, "a.md", "A1");
        write(&root, "b.md", "B1");
        // 第 3 篇的目标父目录被一个普通文件占位 → safe_join 无法建父目录，写入阶段失败
        std::fs::write(root.join("blocked"), "x").unwrap();
        let updates = updates_of(&[("a.md", "A2"), ("b.md", "B2"), ("blocked/c.md", "C1")]);

        assert!(flush_md_updates(&root, &updates).is_err());
        assert_eq!(std::fs::read_to_string(root.join("a.md")).unwrap(), "A1", "前序目标被半写");
        assert_eq!(std::fs::read_to_string(root.join("b.md")).unwrap(), "B1", "前序目标被半写");
        assert!(!root.join("blocked/c.md").exists());
        assert!(tmp_leftovers(&root).is_empty(), "残留：{:?}", tmp_leftovers(&root));
    }

    #[test]
    fn empty_updates_is_noop() {
        let root = TempDir::new("flushmd-empty");
        flush_md_updates(&root, &[]).unwrap();
        assert!(tmp_leftovers(&root).is_empty());
    }
}
