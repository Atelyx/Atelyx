//! 反链与标签索引：纯内存派生，磁盘为真相，按指纹增量刷新以自愈外部改动。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use super::links::{
    at_line_start, backtick_close, backtick_run_at, extract_refs, fence_close_end, fence_len_at,
};
use super::read_dir_filtered;

//
// 设计要点（为多人实时协作预留的红线）：
// - 索引只读派生、绝不落盘仓库（不产生同步冲突文件），永远可从磁盘全量重建（幂等）；
// - 每次查询按 (mtime, size) 指纹 diff 增量刷新：只重读变化/新增的 .md，缺失条目剔除——
//   外部编辑/重命名/删除/文件夹移动全部自愈，无需独立的失效管道（事件驱动索引会漏事件、丢 debounce）；
// - 构建/刷新集中在 refresh_wiki_index 一个函数，未来协作层「内存文档即真相」时只换刷新触发源。
//
// 提取两种链接写法：`[[target]]`/`[[target|别名]]` 与 `[label](path)`（图片 `![..](..)` 不算反链）。

/// 反链行：引用方笔记的相对仓库根路径 + 标题（去扩展名文件名；scan_wiki_backlinks 返回）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklinkRow {
    pub file: String,
    pub title: String,
}

/// 单条提取结果：name 与 path 二选一（`[[..]]` 为 name 形式，`[..](..)` 为 path 形式）。
#[derive(Clone)]
pub(super) struct WikiRef {
    pub(super) name: Option<String>,
    pub(super) path: Option<String>,
}

/// 文件指纹：mtime 毫秒 + 大小（本机本地语义；同步/检出后 mtime 变化 → 自然触发重读自愈）。
#[derive(PartialEq)]
struct FileStamp {
    mtime_ms: u128,
    size: u64,
}

/// 反链索引：rel 路径 → 指纹 + 已提取引用（倒排查询时遍历，纯内存迭代无 I/O，无需预建倒排表）。
#[derive(Default)]
pub struct WikiIndex {
    files: HashMap<String, FileStamp>,
    refs: HashMap<String, Vec<WikiRef>>,
}

/// 增量刷新索引：stat 遍历（快），只重读指纹变化的文件；消失文件从索引剔除。
pub fn refresh_wiki_index(
    root: &Path,
    exclude: &[String],
    index: &mut WikiIndex,
) -> Result<(), String> {
    let mut seen: HashSet<String> = HashSet::new();
    walk_md_in(root, "", exclude, &mut |rel, path| {
        seen.insert(rel.to_string());
        let Some(stamp) = file_stamp(path) else {
            return Ok(());
        };
        if index.files.get(rel).map(|old| *old == stamp).unwrap_or(false) {
            return Ok(());
        }
        // 不可读文件（非 UTF-8/权限）跳过，不中断整仓索引——与其他扫描路径行为一致
        let Ok(content) = std::fs::read_to_string(path) else {
            return Ok(());
        };
        index.files.insert(rel.to_string(), stamp);
        index.refs.insert(rel.to_string(), extract_refs(&content));
        Ok(())
    })?;
    index.files.retain(|rel, _| seen.contains(rel));
    index.refs.retain(|rel, _| seen.contains(rel));
    Ok(())
}

/// 查询反链：`[[name]]` 按笔记名精确匹配；`[label](path)` 按归一化后的完整路径或文件名（basename）匹配
/// （大小写不敏感兜底：Windows 文件系统不区分大小写）。basename 兜底仅在同名唯一时生效——
/// 多处同名时只认精确路径，防给所有同名笔记误记反链。
pub fn query_wiki_backlinks(index: &WikiIndex, note_name: &str, note_file: &str) -> Vec<BacklinkRow> {
    let target_basename = note_file.rsplit('/').next().unwrap_or(note_file);
    // 全仓已索引笔记中同名 basename（大小写不敏感，与下方 eq_ignore_ascii_case 兜底口径一致）恰好一份
    let basename_unique = index
        .files
        .keys()
        .filter(|rel| {
            let base = rel.rsplit('/').next().unwrap_or(rel.as_str());
            base.eq_ignore_ascii_case(target_basename)
        })
        .count()
        == 1;
    let mut rows: Vec<BacklinkRow> = Vec::new();
    for (rel, refs) in &index.refs {
        let hit = refs.iter().any(|r| {
            if let Some(name) = &r.name {
                name == note_name
            } else if let Some(path) = &r.path {
                if path == note_file || path.eq_ignore_ascii_case(note_file) {
                    return true;
                }
                basename_unique && {
                    let base = path.rsplit('/').next().unwrap_or(path.as_str());
                    base == target_basename || base.eq_ignore_ascii_case(target_basename)
                }
            } else {
                false
            }
        });
        if hit {
            let title = rel
                .rsplit('/')
                .next()
                .unwrap_or(rel)
                .trim_end_matches(".md")
                .to_string();
            rows.push(BacklinkRow {
                file: rel.clone(),
                title,
            });
        }
    }
    rows
}

/// 递归遍历仓库 .md（与文件树同过滤：跳过隐藏目录与用户排除文件夹）。
pub fn walk_md_in(
    root: &Path,
    rel: &str,
    exclude: &[String],
    f: &mut dyn FnMut(&str, &Path) -> Result<(), String>,
) -> Result<(), String> {
    if !root.join(rel).exists() {
        return Ok(());
    }
    for (child_rel, is_dir) in read_dir_filtered(root, rel, exclude)? {
        if is_dir {
            walk_md_in(root, &child_rel, exclude, f)?;
        } else if child_rel.ends_with(".md") {
            f(&child_rel, &root.join(&child_rel))?;
        }
    }
    Ok(())
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(FileStamp {
        mtime_ms,
        size: meta.len(),
    })
}

//
// 设计要点：
// - 正文任意处 `#标签` 与 frontmatter `tags` 共同构成仓库标签词汇表（属性区 tags 候选建议的数据源）；
// - 与反链索引同一套指纹增量刷新（mtime+size diff，只重读变化 .md，缺失剔除，外部编辑自愈）；
// - 只扫 .md（不扫 .atlx/对话历史）；标签须含 ≥1 个字母（排除 `#2024`/`#1` 日期序号误判）。
// - 正文提取跳过 frontmatter/围栏代码（含 ≤3 空格缩进）/缩进代码（≥4 空格行首）/行内代码；
//   识别规则：渲染层额外跳过链接内文本（链接内标签仅入索引不渲染，视觉从简）。

/// 标签行：标签名 + 含该标签的笔记数（聚合结果按 count 降序，候选按常用优先）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagRow {
    pub tag: String,
    pub count: u32,
}

/// 标签索引：rel 路径 → 指纹 + 标签集合（聚合遍历纯内存，无 I/O）。
#[derive(Default)]
pub struct TagIndex {
    files: HashMap<String, FileStamp>,
    tags: HashMap<String, HashSet<String>>,
}

/// 增量刷新标签索引：stat 遍历，只重读指纹变化的文件；消失文件剔除。
pub fn refresh_tag_index(
    root: &Path,
    exclude: &[String],
    index: &mut TagIndex,
) -> Result<(), String> {
    let mut seen: HashSet<String> = HashSet::new();
    walk_md_in(root, "", exclude, &mut |rel, path| {
        seen.insert(rel.to_string());
        let Some(stamp) = file_stamp(path) else {
            return Ok(());
        };
        if index.files.get(rel).map(|old| *old == stamp).unwrap_or(false) {
            return Ok(());
        }
        // 不可读文件（非 UTF-8/权限）跳过，不中断整仓索引——与其他扫描路径行为一致
        let Ok(content) = std::fs::read_to_string(path) else {
            return Ok(());
        };
        index.files.insert(rel.to_string(), stamp);
        index.tags.insert(rel.to_string(), extract_note_tags(&content));
        Ok(())
    })?;
    index.files.retain(|rel, _| seen.contains(rel));
    index.tags.retain(|rel, _| seen.contains(rel));
    Ok(())
}

/// 聚合标签计数：count 降序 + 名称升序；不截断——候选清单自带滚动，键入过滤优先于频次。
pub fn aggregate_tag_counts(index: &TagIndex) -> Vec<TagRow> {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for set in index.tags.values() {
        for tag in set {
            *counts.entry(tag.as_str()).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<TagRow> = counts
        .into_iter()
        .map(|(tag, count)| TagRow { tag: tag.to_string(), count })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
    rows
}

/// 提取一篇笔记的全部标签：frontmatter `tags` + 正文内联 `#标签`（去重）。
fn extract_note_tags(content: &str) -> HashSet<String> {
    let mut tags: HashSet<String> = HashSet::new();
    let mut body_start = 0;
    // 仅文档开头 `---` 块内的 `tags` 键算 frontmatter（非开头 `---` 是正文横隔条，不算）
    if let Some(end) = crate::frontmatter::block_end(content) {
        parse_fm_tags(&content[..end], &mut tags);
        body_start = end;
    }
    extract_inline_tags(&content[body_start..], &mut tags);
    tags
}

/// 解析 frontmatter 块的 `tags` 键：`tags: [a, b]`（可跨行）、`tags:\n  - a` 块列表、
/// `tags: foo` 标量。只认顶层键——整块映射可统一缩进，嵌套键与 block scalar 文本里的 `tags:` 不算；
/// 值位的 ` #` 行内注释按 YAML 口径剥离，未闭合的流式数组整体忽略（不造半截标签）。
fn parse_fm_tags(fm: &str, tags: &mut HashSet<String>) {
    // 顶层键缩进：YAML 允许整块映射统一缩进，嵌套键更深——按最小缩进判定顶层
    let root = root_indent(fm);
    let mut lines = fm.lines();
    while let Some(line) = lines.next() {
        if line.len() - line.trim_start_matches(' ').len() != root {
            continue;
        }
        let Some(rest) = line[root..].strip_prefix("tags").map(str::trim_start) else { continue };
        let Some(value) = rest.strip_prefix(':') else { continue };
        let value = strip_comment(value).trim();
        // 值位为空（含只有注释）= 块列表；`[` 开头 = 流式数组；其余按标量处理
        if value.is_empty() {
            collect_block_list(&mut lines, tags);
        } else if let Some(flow) = value.strip_prefix('[') {
            let Some(flow) = collect_flow(flow, &mut lines) else { return };
            for raw in flow.split(',') {
                insert_tag(raw, tags);
            }
        } else {
            insert_tag(value, tags);
        }
        return;
    }
}

/// 顶层键的缩进：非空、非注释、非 `---` 行的最小前导空格数（整块映射可统一缩进）。
fn root_indent(fm: &str) -> usize {
    fm.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with("---")
        })
        .map(|line| line.len() - line.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0)
}

/// 剥离 YAML 行内注释：`#` 前须有空白（`foo#bar` 不算注释），返回 `#` 之前的部分。
fn strip_comment(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b'#' && matches!(bytes[i - 1], b' ' | b'\t') {
            return &raw[..i];
        }
    }
    raw
}

/// 块列表：`tags:` 之后连续的 `- 项` 行（空行跳过，首个非列表行结束；`---` 结束符不算项）。
fn collect_block_list<'a>(lines: &mut impl Iterator<Item = &'a str>, tags: &mut HashSet<String>) {
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Some(rest) = line.trim_start().strip_prefix('-') else { break };
        if !rest.starts_with([' ', '\t']) {
            break;
        }
        insert_tag(rest, tags);
    }
}

/// 收集流式数组（首行 `[` 之后到闭合 `]` 之前，可跨行）；未闭合返回 None。
/// 跨行时按行补分隔逗号：合法 YAML 本就要求逗号，缺失时宁可拆开也不拼成一个标签。
fn collect_flow<'a>(first: &'a str, lines: &mut impl Iterator<Item = &'a str>) -> Option<String> {
    let mut text = String::new();
    let mut rest = first;
    loop {
        if let Some(end) = rest.find(']') {
            text.push_str(&rest[..end]);
            return Some(text);
        }
        text.push_str(strip_comment(rest));
        text.push(',');
        rest = lines.next()?;
    }
}

/// 清洗并收下一个标签值：剥行内注释、引号与首尾空白；空值与 `#` 开头的纯注释段忽略。
fn insert_tag(raw: &str, tags: &mut HashSet<String>) {
    let tag = strip_comment(raw).trim().trim_matches(['"', '\'']).trim();
    if !tag.is_empty() && !tag.starts_with('#') {
        tags.insert(tag.to_string());
    }
}

/// 从正文（已剥 frontmatter）提取内联 `#标签`：跳过围栏代码（含 ≤3 空格缩进）/缩进代码/行内代码；
/// `#` 前一字符非字母/数字/`#`/`_`/`/`（排除 `foo#bar`、`##tag`、URL 片段 `x.com/#faq`），标签须含 ≥1 个字母。
/// 识别规则：渲染层额外跳过链接内文本（标签仅入索引不渲染，视觉从简）。
fn extract_inline_tags(body: &str, tags: &mut HashSet<String>) {
    let bytes = body.as_bytes();
    let len = body.len();
    let mut i = 0;
    while i < len {
        if at_line_start(body, i) {
            // 缩进代码块（≥4 空格行首）：整行跳过（行首 ≥4 空格按代码行处理）
            let rest = &body[i..];
            let lead = rest.bytes().take_while(|&b| b == b' ').count();
            if lead >= 4 {
                let end = rest.find('\n').map_or(len, |p| i + p + 1);
                i = end;
                continue;
            }
            // 围栏代码块：行首或 ≤3 空格缩进（嵌套列表里的围栏；闭合端 fence_close_end 已容忍 ≤3 空格）
            let after = &rest[lead..];
            if let Some(open_len) = fence_len_at(after, 0) {
                let open_char = after.chars().next().unwrap();
                if let Some(close) = fence_close_end(&after[open_len..], open_char, open_len) {
                    i += lead + open_len + close;
                    continue;
                }
            }
        }
        // 行内代码：反引号 run 至等长（或更长）run 闭合
        if bytes[i] == b'`' {
            let n = backtick_run_at(body, i);
            if let Some(close) = backtick_close(body, i + n, n) {
                i = close;
                continue;
            }
        }
        if bytes[i] == b'#' {
            if let Some(tag) = tag_at(body, i) {
                tags.insert(tag.clone());
                i += tag.len() + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// `i` 处若为合法标签起点（`#` 后跟 ≥1 个标签字符且含字母），返回标签名（不含 `#`）。
fn tag_at(body: &str, i: usize) -> Option<String> {
    if let Some(prev) = body[..i].chars().next_back() {
        // `/` 入排除集：URL 片段 `https://x.com/#faq` 的 `#` 前是 `/`，不是标签
        if prev.is_alphanumeric() || prev == '#' || prev == '_' || prev == '/' {
            return None;
        }
    }
    let mut end = 0usize;
    let mut has_alpha = false;
    for ch in body[i + 1..].chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '/' {
            end += ch.len_utf8();
            if ch.is_alphabetic() {
                has_alpha = true;
            }
        } else {
            break;
        }
    }
    if end == 0 || !has_alpha {
        return None;
    }
    Some(body[i + 1..i + 1 + end].to_string())
}

#[cfg(test)]
mod wiki_backlink_tests {
    use super::*;

    /// 反链索引（files = 已索引笔记路径；refs = rel 路径 → 该文件提取到的引用）。
    fn index_with(files: &[&str], refs: &[(&str, Vec<WikiRef>)]) -> WikiIndex {
        let mut index = WikiIndex::default();
        for f in files {
            index
                .files
                .insert(f.to_string(), FileStamp { mtime_ms: 0, size: 0 });
        }
        for (rel, list) in refs {
            index.refs.insert(rel.to_string(), list.clone());
        }
        index
    }

    fn path_ref(target: &str) -> WikiRef {
        WikiRef { name: None, path: Some(target.to_string()) }
    }

    fn name_ref(target: &str) -> WikiRef {
        WikiRef { name: Some(target.to_string()), path: None }
    }

    #[test]
    fn basename_fallback_only_when_unique() {
        // 同名 basename 多处：裸文件名链接无法消歧，只认精确路径，不给所有同名笔记误记反链
        let index = index_with(
            &["a/note.md", "b/note.md"],
            &[("a/note.md", vec![path_ref("c/note.md")])],
        );
        let rows = query_wiki_backlinks(&index, "note", "a/note.md");
        assert!(rows.is_empty(), "同名多处时裸名链接不应记为 a/note.md 的反链");

        // 精确路径链接不受唯一性影响
        let index = index_with(
            &["a/note.md", "b/note.md"],
            &[("b/note.md", vec![path_ref("a/note.md")])],
        );
        let rows = query_wiki_backlinks(&index, "note", "a/note.md");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file, "b/note.md");
    }

    #[test]
    fn basename_fallback_hits_when_single_note_shares_name() {
        // 全仓唯一 basename：裸文件名链接消歧成功，计入反链
        let index = index_with(
            &["docs/only.md", "other.md"],
            &[("other.md", vec![path_ref("only.md")])],
        );
        let rows = query_wiki_backlinks(&index, "only", "docs/only.md");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file, "other.md");
    }

    #[test]
    fn basename_uniqueness_is_case_insensitive() {
        // 大小写不同但同名（Windows 视为同名）：仍按多处处理，裸名兜底不生效
        let index = index_with(
            &["a/Note.md", "b/note.md"],
            &[("b/note.md", vec![path_ref("x/note.md")])],
        );
        let rows = query_wiki_backlinks(&index, "Note", "a/Note.md");
        assert!(rows.is_empty(), "跨大小写同名多处时裸名链接不应计入");
    }

    #[test]
    fn wiki_name_form_stays_exact() {
        // `[[name]]` 保持精确匹配（既有语义，防回归）
        let index = index_with(
            &["a/note.md"],
            &[("a/note.md", vec![name_ref("note")])],
        );
        assert_eq!(query_wiki_backlinks(&index, "note", "a/note.md").len(), 1);
        assert!(query_wiki_backlinks(&index, "other", "a/note.md").is_empty());
    }
    #[test]
    fn code_block_and_frontmatter_links_are_not_backlinks() {
        // 反链与改写同口径：只在正文里算链接，代码块与 frontmatter 的数据不算
        let root = crate::vault::test_support::TempDir::new("wiki-scan-scope");
        std::fs::write(root.join("目标.md"), "目标笔记").unwrap();
        std::fs::write(
            root.join("代码块.md"),
            "---\ntags: [[目标]]\n---\n\n```\n[[目标]]\n[目标](目标.md)\n```\n",
        )
        .unwrap();
        std::fs::write(root.join("正文.md"), "见 [[目标]] 与 [目标](目标.md)").unwrap();

        let mut index = WikiIndex::default();
        refresh_wiki_index(&root, &[], &mut index).unwrap();
        let mut files: Vec<String> = query_wiki_backlinks(&index, "目标", "目标.md")
            .into_iter()
            .map(|row| row.file)
            .collect();
        files.sort();
        assert_eq!(files, vec!["正文.md"], "代码块与 frontmatter 内的链接不得计入反链");
    }
}

#[cfg(test)]
mod tag_index_tests {
    use super::*;

    /// 断言提取结果集合与期望一致（顺序无关）。
    fn assert_tags(content: &str, expect: &[&str]) {
        let got = extract_note_tags(content);
        let want: HashSet<String> = expect.iter().map(|s| s.to_string()).collect();
        assert_eq!(got, want, "content: {:?}", content);
    }

    #[test]
    fn inline_and_frontmatter_union() {
        // frontmatter 内联数组 + 正文内联标签
        assert_tags(
            "---\ntags: [a, b]\n---\n正文 #c 和 #a\n",
            &["a", "b", "c"],
        );
    }

    #[test]
    fn frontmatter_block_list() {
        assert_tags(
            "---\ntags:\n  - x\n  - y\n---\nbody\n",
            &["x", "y"],
        );
    }

    #[test]
    fn heading_and_number_not_tag() {
        // 标题 `# 标题`/`###`、日期 `#2024`、序号 `#1` 都不是标签
        assert_tags("# 标题\n## 子标题\n#2024 和 #1\n", &[]);
    }

    #[test]
    fn code_blocks_and_inline_code_skipped() {
        assert_tags(
            "前 #real\n```\n#code-in-fence\n```\n`#inline` #after\n",
            &["real", "after"],
        );
    }

    #[test]
    fn word_boundary_and_double_hash() {
        // `foo#bar`、`##tag` 排除；`#a#b` 只取 `a`
        assert_tags("foo#bar ##tag #a#b\n", &["a"]);
    }

    #[test]
    fn chinese_and_nested_tags() {
        // 中文标签与层级标签
        assert_tags("#重要 #项目/前端\n", &["重要", "项目/前端"]);
    }

    #[test]
    fn body_only_without_frontmatter() {
        assert_tags("只有正文 #tag\n", &["tag"]);
    }

    #[test]
    fn frontmatter_not_at_start_is_hr() {
        // 非开头的 `---` 是横隔条，不是 frontmatter；其后 `#tag` 仍算正文标签
        assert_tags("正文\n---\n#tag\n", &["tag"]);
    }

    #[test]
    fn scalar_and_inline_comment() {
        // `tags: foo` 标量；` # 注释` 剥离（`foo#bar` 无空格前缀不算注释）
        assert_tags("---\ntags: 标签\n---\n", &["标签"]);
        assert_tags("---\ntags: a # todo\n---\n", &["a"]);
        assert_tags("---\ntags:\n  - x # 备注\n  - y\n---\n", &["x", "y"]);
        assert_tags("---\ntags: foo#bar\n---\n", &["foo#bar"]);
    }

    #[test]
    fn crlf_frontmatter() {
        assert_tags("---\r\ntags: [a, b]\r\n---\r\n正文 #c\r\n", &["a", "b", "c"]);
    }

    #[test]
    fn indented_code_blocks_skipped() {
        // ≥4 空格缩进行按代码处理；≤3 空格缩进围栏（嵌套列表）整块跳过
        assert_tags("正文\n    #indented\n#real\n", &["real"]);
        assert_tags("列表项\n  ~~~js\n  #code\n  ~~~\n#real\n", &["real"]);
    }

    #[test]
    fn url_fragment_not_tag() {
        // URL 片段 `#` 前是 `/`，不是标签
        assert_tags("见 https://x.com/#faq\n", &[]);
    }

    #[test]
    fn frontmatter_comment_only_value_is_empty() {
        // `tags: # 备注` 只有注释：既不是标量标签，也不造出 `# 备注`
        assert_tags("---\ntags: # 备注\n---\n", &[]);
    }

    #[test]
    fn frontmatter_comment_then_block_list() {
        // 值位只有注释时后续 `- 项` 仍是块列表
        assert_tags("---\ntags: # 备注\n  - a\n  - b\n---\n", &["a", "b"]);
    }

    #[test]
    fn frontmatter_indented_root_mapping() {
        // YAML 允许整块映射统一缩进：缩进与顶层一致时仍是顶层 `tags`
        assert_tags("---\n  tags: [a]\n---\n", &["a"]);
        assert_tags("---\n  tags:\n    - a\n---\n", &["a"]);
    }

    #[test]
    fn frontmatter_nested_tags_ignored() {
        // 只认列首顶层键：缩进嵌套键与 block scalar 文本里的 `tags:` 都不算
        assert_tags("---\nmeta:\n  tags: [x]\n---\n", &[]);
        assert_tags("---\nnote: |\n  tags: [y]\n---\n", &[]);
    }

    #[test]
    fn frontmatter_unclosed_flow_ignored() {
        // 未闭合的流式数组（YAML 非法）整体忽略，不留下 `[a, b` 这类半截标签
        assert_tags("---\ntags: [a, b\n---\n", &[]);
    }

    #[test]
    fn frontmatter_multiline_flow_array() {
        assert_tags("---\ntags: [\n  a, # 一\n  b\n]\n---\n", &["a", "b"]);
    }

    #[test]
    fn aggregate_keeps_every_tag_sorted_by_count() {
        let mut index = TagIndex::default();
        for i in 0..1004 {
            let mut set = HashSet::new();
            set.insert(format!("t{i:04}"));
            index.tags.insert(format!("f{i}.md"), set);
        }
        let mut hot = HashSet::new();
        hot.insert("hot".to_string());
        hot.insert("t0000".to_string());
        index.tags.insert("a.md".to_string(), hot);
        index.tags.insert("b.md".to_string(), HashSet::from(["hot".to_string()]));
        let rows = aggregate_tag_counts(&index);
        assert_eq!(rows.len(), 1005, "候选池不截断");
        assert_eq!(rows[0].tag, "hot", "按含该标签的笔记数降序");
        assert_eq!(rows[0].count, 2);
        let rest: Vec<&str> = rows[1..].iter().map(|r| r.tag.as_str()).collect();
        let mut sorted = rest.clone();
        sorted.sort_unstable();
        assert_eq!(rest, sorted, "同频按名称升序");
    }

    /// 两端共用 `fixtures/tag-parse.json`：`cases` 是常规形态，`alignedCases` 是
    /// frontmatter 定界边界（BOM、非规范分隔行），期望值取自客户端；两端必须切在同一位置。
    /// 任一形态两端提取结果都必须与 tags 完全一致（顺序无关）。
    #[test]
    fn tag_parse_fixture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/tag-parse.json");
        let raw = std::fs::read_to_string(path)
            .expect("夹具缺失：fixtures/tag-parse.json 应随源码入库");
        let fixture: serde_json::Value = serde_json::from_str(&raw).expect("夹具不是合法 JSON");
        let cases = fixture["cases"].as_array().expect("夹具缺 cases 数组");
        let aligned = fixture["alignedCases"]["cases"]
            .as_array()
            .expect("夹具缺 alignedCases.cases 数组");
        assert!(cases.len() >= 25, "语料规模兜底：cases 疑似被清空");
        assert!(aligned.len() >= 8, "语料规模兜底：alignedCases 疑似被清空");
        for case in cases.iter().chain(aligned) {
            let name = case["name"].as_str().unwrap_or_default();
            let content = case["content"].as_str().expect("语料缺 content");
            let mut want: Vec<String> = case["tags"]
                .as_array()
                .expect("语料缺 tags")
                .iter()
                .map(|tag| tag.as_str().expect("tags 项须为字符串").to_string())
                .collect();
            let mut got: Vec<String> = extract_note_tags(content).into_iter().collect();
            got.sort();
            want.sort();
            assert_eq!(got, want, "语料 {name} 标签不一致：content = {content:?}");
        }
    }
}
