//! 派生索引与扫描（真源 = 空间内容文件树）：反链 / 标签 / glob / grep。
//! 与客户端检索同一套语义与同一批引擎（globset + regex）；反链 / 标签索引为纯内存只读派生，
//! 按文件指纹（mtime 毫秒 + 大小）增量刷新、消失文件剔除（见 `refresh_wiki_index` / `SpaceIndexCache::with_space`）。

use std::collections::{HashMap, HashSet};
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use globset::{GlobBuilder, GlobMatcher};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::access::member_root;
use crate::fsops::{blocking, file_mtime_secs, percent_decode, walk_md_in, SpaceRoot};
use crate::state::{lock_recover, ServerState};
use crate::{ApiError, ApiResult};

// ===== 索引缓存（进程内，随空间内容重建，不持久化） =====

/// 单空间的派生索引（反链 + 标签）。`exclude_folders` 记录构建本缓存时的团队排除名单：
/// 名单变化时整份作废重建（否则被排除目录的陈旧条目会残留在索引里）。
#[derive(Default)]
pub struct SpaceIndex {
    wiki: WikiIndex,
    tags: TagIndex,
    exclude_folders: Vec<String>,
    /// 是否有过一次成功刷新：没有成功过的缓存不能当作「没有反链 / 没有标签」对外回答。
    refreshed: bool,
}

// ===== 反链索引 =====

/// 反链行：引用方笔记的相对路径 + 标题。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(Debug))]
pub struct BacklinkRow {
    pub file: String,
    pub title: String,
}

/// 单条提取结果：name 与 path 二选一（`[[..]]` 为 name 形式，`[..](..)` 为 path 形式）。
#[derive(Clone, Debug)]
struct WikiRef {
    name: Option<String>,
    path: Option<String>,
}

/// 文件指纹：mtime 毫秒 + 大小。
#[derive(Clone, PartialEq)]
struct FileStamp {
    mtime_ms: u128,
    size: u64,
}

/// 反链索引：rel 路径 → 指纹 + 已提取引用（倒排查询时遍历，纯内存迭代无 I/O）。
#[derive(Clone, Default)]
struct WikiIndex {
    files: HashMap<String, FileStamp>,
    refs: HashMap<String, Vec<WikiRef>>,
}

/// 增量刷新：stat 遍历（快），只重读指纹变化的文件；消失文件剔除。
fn refresh_wiki_index(root: &FsPath, exclude_folders: &[String], index: &mut WikiIndex) -> Result<(), String> {
    let mut seen: HashSet<String> = HashSet::new();
    walk_md_in(root, "", exclude_folders, &mut |rel, path| {
        seen.insert(rel.to_string());
        let Some(stamp) = file_stamp(path) else {
            return Ok(());
        };
        if index.files.get(rel).map(|old| *old == stamp).unwrap_or(false) {
            return Ok(());
        }
        // 不可读文件（非 UTF-8/权限）跳过，不中断整仓索引
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

/// 查询反链：`[[name]]` 按笔记名精确匹配；`[label](path)` 按归一化后的完整路径或文件名
/// （basename）匹配（大小写不敏感兜底）。basename 兜底仅在同名唯一时生效——
/// 多处同名时只认精确路径，防给所有同名笔记误记反链。
fn query_wiki_backlinks(index: &WikiIndex, note_name: &str, note_file: &str) -> Vec<BacklinkRow> {
    let target_basename = note_file.rsplit('/').next().unwrap_or(note_file);
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
            let title = rel.rsplit('/').next().unwrap_or(rel).trim_end_matches(".md").to_string();
            rows.push(BacklinkRow { file: rel.clone(), title });
        }
    }
    rows
}

fn file_stamp(path: &FsPath) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    Some(FileStamp { mtime_ms, size: meta.len() })
}

// ===== 标签索引 =====

/// 标签行：标签名 + 全空间出现次数。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagRow {
    pub tag: String,
    pub count: u32,
}

/// 标签索引：rel 路径 → 指纹 + 标签集合。
#[derive(Clone, Default)]
struct TagIndex {
    files: HashMap<String, FileStamp>,
    tags: HashMap<String, HashSet<String>>,
}

fn refresh_tag_index(root: &FsPath, exclude_folders: &[String], index: &mut TagIndex) -> Result<(), String> {
    let mut seen: HashSet<String> = HashSet::new();
    walk_md_in(root, "", exclude_folders, &mut |rel, path| {
        seen.insert(rel.to_string());
        let Some(stamp) = file_stamp(path) else {
            return Ok(());
        };
        if index.files.get(rel).map(|old| *old == stamp).unwrap_or(false) {
            return Ok(());
        }
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

/// 聚合标签计数：count 降序 + 名称升序；不截断（客户端候选清单自带滚动）。
fn aggregate_tag_counts(index: &TagIndex) -> Vec<TagRow> {
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
/// 标签须含 ≥1 个字母（排除 `#2024` 日期序号误判）；正文提取跳过 frontmatter /
/// 围栏代码（含 ≤3 空格缩进）/ 缩进代码（≥4 空格行首）/ 行内代码。
fn extract_note_tags(content: &str) -> HashSet<String> {
    let mut tags: HashSet<String> = HashSet::new();
    let mut body_start = 0;
    // 仅文档开头 `---` 块内的 `tags` 键算 frontmatter（非开头 `---` 是正文横隔条，不算）
    if let Some(end) = frontmatter_end(content) {
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

/// 顶层键的缩进：非空、非注释、非分隔行（`---` / 带 BOM 的 `\u{feff}---`）的最小前导空格数
/// （整块映射可统一缩进）。
fn root_indent(fm: &str) -> usize {
    fm.lines()
        .filter(|line| {
            let trimmed = line.trim().trim_start_matches('\u{feff}');
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

/// 从正文（已剥 frontmatter）提取内联 `#标签`：跳过围栏代码 / 缩进代码 / 行内代码；
/// `#` 前一字符非字母/数字/`#`/`_`/`/`（排除 `foo#bar`、`##tag`、URL 片段 `x.com/#faq`）。
fn extract_inline_tags(body: &str, tags: &mut HashSet<String>) {
    let bytes = body.as_bytes();
    let len = body.len();
    let mut i = 0;
    while i < len {
        if at_line_start(body, i) {
            // 缩进代码块（≥4 空格行首）：整行跳过
            let rest = &body[i..];
            let lead = rest.bytes().take_while(|&b| b == b' ').count();
            if lead >= 4 {
                let end = rest.find('\n').map_or(len, |p| i + p + 1);
                i = end;
                continue;
            }
            // 围栏代码块：行首或 ≤3 空格缩进（嵌套列表里的围栏）
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

/// 提取引用：`[[target]]` / `[[target|别名]]` + `[label](path)`（回溯 `[` 检查前导 `!` 排除图片）。
fn extract_refs(content: &str) -> Vec<WikiRef> {
    let mut refs = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                let target = &after[..end];
                let name = target.split('|').next().unwrap_or(target).trim();
                if !name.is_empty() {
                    refs.push(WikiRef { name: Some(name.to_string()), path: None });
                }
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    let mut rest = content;
    while let Some(rel) = rest.find("](") {
        let before = &rest[..rel];
        let after = &rest[rel + 2..];
        let close = after.find(')').unwrap_or(after.len());
        let path = &after[..close];
        // 无前导 `[`（正文裸写 `](`）不是链接；回溯 `[` 检查前导 `!` 排除图片
        let Some(open) = before.rfind('[') else {
            rest = &after[close..];
            continue;
        };
        let is_image = open > 0 && before.as_bytes()[open - 1] == b'!';
        if !is_image {
            if let Some(normalized) = normalize_link_path(path) {
                refs.push(WikiRef { name: None, path: Some(normalized) });
            }
        }
        rest = &after[close..];
    }
    refs
}

/// 归一化链接路径：percent 解码、反斜杠→`/`、去 `./` 与前导 `/`；含 `..` 段返回 None（防越出）。
fn normalize_link_path(raw: &str) -> Option<String> {
    let mut s = percent_decode(raw);
    s = s.replace('\\', "/");
    while let Some(stripped) = s.strip_prefix("./") {
        s = stripped.to_string();
    }
    while let Some(stripped) = s.strip_prefix('/') {
        s = stripped.to_string();
    }
    if s.is_empty() {
        return None;
    }
    for seg in s.split('/') {
        if seg == ".." {
            return None;
        }
    }
    Some(s)
}

/// 若 `i` 处位于行首（i == 0 或前一字符为换行）。
fn at_line_start(content: &str, i: usize) -> bool {
    i == 0 || content.as_bytes()[i - 1] == b'\n'
}

/// `i` 处若为围栏行首（``` 或 ~~~，≥3 个同字符），返回围栏长度。
fn fence_len_at(content: &str, i: usize) -> Option<usize> {
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
fn fence_close_end(rest: &str, open_char: char, open_len: usize) -> Option<usize> {
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

/// 文档开头 frontmatter 块结束位置（含闭合行行尾换行）；无块或未闭合返回 None。
/// 定界与客户端 `frontmatter::block_end` 同口径：BOM 跳过、起始行以 `---` 起始即可（其后可有尾注）、
/// 闭合行 = 其后第一条以 `---` 起始的行。两端必须切在同一位置，否则 frontmatter 正文会被当正文扫描。
fn frontmatter_end(content: &str) -> Option<usize> {
    let open_end = content.find('\n')?;
    if !content[..open_end].trim_start_matches('\u{feff}').starts_with("---") {
        return None;
    }
    let mut pos = open_end;
    for line in content[open_end..].split_inclusive('\n') {
        pos += line.len();
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
        if trimmed.starts_with("---") {
            return Some(pos);
        }
    }
    None
}

/// 反引号 run 长度（i 处起连续反引号数）。
fn backtick_run_at(content: &str, i: usize) -> usize {
    content[i..].bytes().take_while(|&b| b == b'`').count()
}

/// 从 `from` 起找 ≥n 个连续反引号的闭合位置（run 之后）；无闭合返回 None。
fn backtick_close(content: &str, from: usize, n: usize) -> Option<usize> {
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

// ===== glob / grep =====

/// glob 单次内联返回的路径上限（超限附 total，调用方提示收窄）。
const GLOB_MAX_RESULTS: usize = 100;
/// grep 单次内联返回的匹配上限。
const GREP_MAX_MATCHES: usize = 250;
/// 单行预览的字节上限（截断保持 UTF-8 字符边界）。
const GREP_MAX_LINE_BYTES: usize = 2000;
/// 二进制探测窗口：前 N 字节含 `\0` 判二进制跳过。
const GREP_BINARY_PROBE_BYTES: usize = 8192;
/// 单个文件的最大读取字节数：超大文件跳过。
const GREP_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// grep 结果聚合字节预算：超限停止保留（total 仍精确计数）。
const GREP_MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GrepMatchRow {
    path: String,
    line_number: usize,
    line: String,
}

/// 构建 glob 匹配器：模式不含「/」时自动前缀 `**/`（匹配任意深度的文件名）；
/// `literal_separator(true)`：`*`/`?`/字符类不跨 `/`，仅 `**` 可跨层。
fn build_glob_matcher(pattern: &str) -> Result<GlobMatcher, String> {
    let normalized = if pattern.contains('/') { pattern.to_string() } else { format!("**/{pattern}") };
    let glob = GlobBuilder::new(&normalized)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("glob 模式无效：{e}"))?;
    Ok(glob.compile_matcher())
}

/// 归一化 path 参数为规范相对路径（`\` → `/`、去尾斜杠、去 `./` 前缀、折叠中间 `//`；空/`.` = 根）。
fn normalize_base_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while s.ends_with('/') {
        s.pop();
    }
    while s.starts_with("./") {
        s.drain(0..2);
    }
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    if s == "." {
        String::new()
    } else {
        s
    }
}

/// 解析 `path` 参数：缺省 = 根（目录）；给出 = 归一化 + 安全校验后按「文件/目录」区分。
/// 指向隐藏目录（. 开头段）拒绝（检索层完全屏蔽隐藏内容）。
fn resolve_base(root: &SpaceRoot, path: Option<&str>) -> Result<(String, bool), String> {
    let p = match path {
        None => return Ok((String::new(), false)),
        Some(p) => normalize_base_path(p),
    };
    if p.is_empty() {
        return Ok((String::new(), false));
    }
    if crate::fsops::has_hidden_segment(&p) {
        return Err("路径位于隐藏目录，检索不可访问".to_string());
    }
    let abs = root.join(&p, false).map_err(|e| e.to_string())?;
    let meta = std::fs::metadata(&abs).map_err(|_| format!("路径不存在或不可访问：{p}"))?;
    Ok((p, meta.is_file()))
}

/// 按 glob 模式收集命中文件（单文件基准时只判定该文件）。
fn collect_glob_files(
    root: &FsPath,
    base_rel: &str,
    base_is_file: bool,
    matcher: &GlobMatcher,
    exclude_folders: &[String],
) -> Result<Vec<(String, i64)>, String> {
    if base_is_file {
        let mut out = Vec::new();
        if matcher.is_match(base_rel) {
            out.push((base_rel.to_string(), file_mtime_secs(&root.join(base_rel))));
        }
        return Ok(out);
    }
    let mut out = Vec::new();
    crate::fsops::walk_files(root, base_rel, exclude_folders, &mut out)?;
    out.retain(|(rel, _)| matcher.is_match(rel));
    Ok(out)
}

/// 前 N 字节含 `\0` 判二进制（跳过不可读内容避免垃圾回填）。
fn is_binary(bytes: &[u8]) -> bool {
    let probe_len = bytes.len().min(GREP_BINARY_PROBE_BYTES);
    bytes[..probe_len].contains(&0)
}

/// 按字节上限截断单行预览，落在 UTF-8 字符边界，超长附后缀提示。
fn preview_line(line: &str, max_bytes: usize) -> String {
    if line.len() <= max_bytes {
        return line.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}... (line truncated)", &line[..cut])
}

/// 纯函数：对一段文本逐行正则匹配（剥离尾 `\r`、按 1-based 绝对行号），
/// 保留前 GREP_MAX_MATCHES 条且受聚合字节预算约束、计数全部命中。
fn scan_content(
    content: &str,
    rel: &str,
    re: &Regex,
    retained: &mut Vec<GrepMatchRow>,
    total: &mut usize,
) {
    let mut out_bytes = 0usize;
    for (idx, raw) in content.split_terminator('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if !re.is_match(line) {
            continue;
        }
        *total += 1;
        if retained.len() >= GREP_MAX_MATCHES {
            continue;
        }
        let preview = preview_line(line, GREP_MAX_LINE_BYTES);
        if out_bytes + preview.len() > GREP_MAX_OUTPUT_BYTES {
            continue;
        }
        out_bytes += preview.len();
        retained.push(GrepMatchRow {
            path: rel.to_string(),
            line_number: idx + 1,
            line: preview,
        });
    }
}

/// 扫描单个文件：不可读/超大/二进制跳过，其余按行匹配。
fn scan_file(root: &FsPath, rel: &str, re: &Regex, retained: &mut Vec<GrepMatchRow>, total: &mut usize) {
    let abs = root.join(rel);
    let meta = match std::fs::metadata(&abs) {
        Ok(m) => m,
        Err(_) => return,
    };
    if meta.len() > GREP_MAX_FILE_BYTES {
        return;
    }
    let bytes = match std::fs::read(&abs) {
        Ok(b) => b,
        Err(_) => return,
    };
    if is_binary(&bytes) {
        return;
    }
    let content = String::from_utf8_lossy(&bytes);
    scan_content(&content, rel, re, retained, total);
}

/// 递归搜索目录树内文件（可选 include 正向 glob 过滤），单文件基准时只搜该文件。
fn scan_for_matches(
    root: &FsPath,
    base_rel: &str,
    is_file: bool,
    re: &Regex,
    include: Option<&GlobMatcher>,
    exclude_folders: &[String],
    retained: &mut Vec<GrepMatchRow>,
    total: &mut usize,
) -> Result<(), String> {
    if is_file {
        if include.map(|m| m.is_match(base_rel)).unwrap_or(true) {
            scan_file(root, base_rel, re, retained, total);
        }
        return Ok(());
    }
    let mut files = Vec::new();
    crate::fsops::walk_files(root, base_rel, exclude_folders, &mut files)?;
    for (rel, _) in files {
        if include.map(|m| m.is_match(&rel)).unwrap_or(true) {
            scan_file(root, &rel, re, retained, total);
        }
    }
    Ok(())
}

// ===== HTTP 端点 =====

/// 刷新失败后的冷却窗口：窗口内不再重扫，直接沿用上次成功结果（坏盘期间不制造重试风暴）。
const FAILED_RETRY_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// 单个空间的刷新节流状态：失败时刻 + 失败原因（成功即清空）。
#[derive(Default, Clone)]
struct RefreshFailure {
    /// 上次刷新失败的时刻（None = 上次刷新成功）。
    at: Option<std::time::Instant>,
    reason: Option<String>,
}

/// 空间派生索引缓存（space_id → 反链/标签索引；纯内存只读派生，可随时重建，不落盘）。
/// 持有方 = `ServerState`；本类型自足封装取用与刷新，不反向依赖状态模块。
pub(crate) struct SpaceIndexCache {
    caches: Mutex<HashMap<String, SpaceIndex>>,
    /// 各空间刷新失败的时刻：TTL 内不再重扫（冷却），TTL 过后允许重试。
    failures: Mutex<HashMap<String, RefreshFailure>>,
    /// 刷新串行化：刷新要搬出索引在阻塞池里做（不占 async 执行器），
    /// 同一时刻只允许一次刷新在途，避免并发请求各自全量扫描同一空间。
    refresh: tokio::sync::Mutex<()>,
    /// 注入的刷新实现（测试可替换成必失败并计数）；None = 真实文件系统刷新。
    refresher: Option<Arc<RefreshFn>>,
    /// 刷新失败后的冷却时长（生产取 `FAILED_RETRY_TTL`；测试可调短）。
    retry_ttl: std::time::Duration,
}

/// 注入的刷新实现签名：测试替身与真实刷新同接口，便于断言「冷却期内不再重扫」。
type RefreshFn = dyn Fn(&FsPath, &[String], &mut WikiIndex, &mut TagIndex) -> Result<(), String> + Send + Sync;

/// 手写 Default 转调 `new`：派生 Default 会把 `retry_ttl` 置零，冷却窗口随之失效。
impl Default for SpaceIndexCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SpaceIndexCache {
    pub(crate) fn new() -> Self {
        Self {
            caches: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            refresh: tokio::sync::Mutex::new(()),
            refresher: None,
            retry_ttl: FAILED_RETRY_TTL,
        }
    }

    /// 测试用：替换刷新实现（返回 Err 即模拟刷新失败，调用次数可计数）并调短冷却时长。
    #[cfg(test)]
    fn with_refresher(refresher: Arc<RefreshFn>, retry_ttl: std::time::Duration) -> Self {
        Self { refresher: Some(refresher), retry_ttl, ..Self::new() }
    }

    /// 取（或懒建）空间的派生索引并增量刷新后在 `f` 内访问。刷新是 stat 遍历 + 变化文件重读，
    /// 在阻塞池里跑（不占 async 执行器）；刷新期间不持缓存锁，其他空间的查询不被拖住。
    ///
    /// 失败语义（契约）：
    /// - 刷新失败保留上一次的成功结果——宁可暂时陈旧，也不能把索引变成「全仓无引用」；
    /// - 只有成功刷新过的空间才进冷却：失败后在 `FAILED_RETRY_TTL` 内沿用旧缓存、不再重扫
    ///   （坏盘期间不制造重试风暴）；
    /// - 从没成功过的空间刷新失败一律回错误（端点 → 500），不返回空索引——空索引会被读成
    ///   「没有反链 / 没有标签」，等于把「读不出来」说成「本来就没有」；
    /// - 失败期间以**旧排除名单**回答：名单刚改又恰好刷不出来时，本 TTL 窗口内被排除目录
    ///   的反链/标签仍可见，过期后重新按新名单重建并收敛。
    pub(crate) async fn with_space<R>(
        &self,
        state: &ServerState,
        space_id: &str,
        root: &FsPath,
        f: impl FnOnce(&mut SpaceIndex) -> R,
    ) -> Result<R, ApiError> {
        let exclude = crate::meta::space_exclusions(state, space_id).await;
        // 冷却窗口内直接返回旧缓存：不取刷新锁、不重扫（只有成功过的空间才有旧缓存可给）
        if let Some(reason) = self.cooling_down(space_id) {
            tracing::debug!(space_id = %space_id, "索引刷新冷却中，沿用上次结果：{reason}");
            let mut caches = lock_recover(&self.caches);
            let index = caches.entry(space_id.to_string()).or_default();
            return Ok(f(index));
        }
        // 刷新串行化：搬出索引 → 阻塞池刷新 → 成功后写回，同一空间不并发全量扫描
        let _refresh = self.refresh.lock().await;
        // 等锁期间可能已被前一个持有者刷新过（成功或失败）：再看一次冷却状态
        if let Some(reason) = self.cooling_down(space_id) {
            tracing::debug!(space_id = %space_id, "索引刷新冷却中，沿用上次结果：{reason}");
            let mut caches = lock_recover(&self.caches);
            let index = caches.entry(space_id.to_string()).or_default();
            return Ok(f(index));
        }
        // 搬出的是「副本」：刷新期间缓存里保留旧索引，取消或刷新失败都不会让该空间索引变空。
        // 排除名单变化时从空索引重建（旧条目指向已排除目录）；重建失败则沿用旧缓存（含旧名单）。
        // has_success 只看这份缓存是否成功刷新过：名单变化不改变「有没有可沿用的旧结果」。
        let (mut wiki, mut tags, has_success) = {
            let mut caches = lock_recover(&self.caches);
            let index = caches.entry(space_id.to_string()).or_default();
            let has_success = index.refreshed;
            if index.exclude_folders != exclude {
                (WikiIndex::default(), TagIndex::default(), has_success)
            } else {
                (index.wiki.clone(), index.tags.clone(), has_success)
            }
        };
        // 刷新（真实遍历或注入替身）统一进阻塞池：不让文件遍历占住 async 执行器
        let refreshed = blocking("索引刷新", {
            let refresher = self.refresher.clone();
            let root = root.to_path_buf();
            let exclude = exclude.clone();
            move || {
                let outcome = match &refresher {
                    Some(refresher) => refresher(&root, &exclude, &mut wiki, &mut tags),
                    None => refresh_both(&root, &exclude, &mut wiki, &mut tags),
                };
                Ok((outcome, wiki, tags))
            }
        })
        .await;
        let mut caches = lock_recover(&self.caches);
        let mut failures = lock_recover(&self.failures);
        let reason = match refreshed {
            // 刷新成功才替换；顺带清掉冷却记录
            Ok((Ok(()), wiki, tags)) => {
                let index = caches.entry(space_id.to_string()).or_default();
                index.wiki = wiki;
                index.tags = tags;
                index.exclude_folders = exclude;
                index.refreshed = true;
                failures.remove(space_id);
                return Ok(f(index));
            }
            // 阻塞任务本身失败（panic / 线程池关闭）与刷新失败同一条处置
            Ok((Err(reason), _, _)) => reason,
            Err(e) => e.1,
        };
        tracing::warn!(space_id = %space_id, "索引刷新失败：{reason}");
        if !has_success {
            // 从没成功过的空间没有可沿用的旧结果：回错误，别让空索引冒充「没有反链 / 没有标签」
            return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, reason));
        }
        // 有过成功缓存：记冷却后沿用旧索引与旧名单（契约见函数头），窗口内不再重扫
        failures.insert(
            space_id.to_string(),
            RefreshFailure { at: Some(std::time::Instant::now()), reason: Some(reason) },
        );
        let index = caches.entry(space_id.to_string()).or_default();
        Ok(f(index))
    }

    /// 冷却窗口内返回上次失败原因（None = 可以尝试刷新）。
    fn cooling_down(&self, space_id: &str) -> Option<String> {
        let failures = lock_recover(&self.failures);
        let failure = failures.get(space_id)?;
        let at = failure.at?;
        if at.elapsed() < self.retry_ttl {
            Some(failure.reason.clone().unwrap_or_else(|| "上次刷新失败".to_string()))
        } else {
            None
        }
    }
}

/// 真实刷新：两个索引各自尽力跑完，任一失败即合并上报（失败原因带是哪一侧）。
fn refresh_both(
    root: &FsPath,
    exclude: &[String],
    wiki: &mut WikiIndex,
    tags: &mut TagIndex,
) -> Result<(), String> {
    let mut failure: Option<String> = None;
    if let Err(e) = refresh_wiki_index(root, exclude, wiki) {
        failure = Some(format!("反链索引刷新失败：{e}"));
    }
    if let Err(e) = refresh_tag_index(root, exclude, tags) {
        failure.get_or_insert(format!("标签索引刷新失败：{e}"));
    }
    match failure {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklinksQuery {
    note_name: String,
    note_file: String,
}

pub async fn backlinks(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Query(query): Query<BacklinksQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let rows = state
        .index_cache()
        .with_space(&state, &space_id, &root.0, |index| {
            query_wiki_backlinks(&index.wiki, &query.note_name, &query.note_file)
        })
        .await?;
    Ok(Json(serde_json::to_value(rows).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?))
}

pub async fn tags(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let rows = state
        .index_cache()
        .with_space(&state, &space_id, &root.0, |index| aggregate_tag_counts(&index.tags))
        .await?;
    Ok(Json(serde_json::to_value(rows).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobBody {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

pub async fn glob(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<GlobBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let matcher = build_glob_matcher(&body.pattern).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    let exclude = crate::meta::space_exclusions(&state, &space_id).await;
    let requested = body.path.clone();
    // 基准路径解析与全树遍历都是阻塞 IO：整段放进阻塞池
    let (root_dir, matcher_for_io) = (root.0.clone(), matcher.clone());
    let entries = blocking("glob 扫描", move || {
        let (base_rel, base_is_file) = resolve_base(&SpaceRoot(root_dir.clone()), requested.as_deref())
            .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
        let mut entries = collect_glob_files(&root_dir, &base_rel, base_is_file, &matcher_for_io, &exclude)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(entries)
    })
    .await?;
    let total = entries.len();
    let capped = total > GLOB_MAX_RESULTS;
    let paths: Vec<String> = entries.into_iter().take(GLOB_MAX_RESULTS).map(|(rel, _)| rel).collect();
    Ok(Json(json!({ "root": body.path.unwrap_or_default(), "paths": paths, "total": total, "capped": capped })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepBody {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
}

/// grep 端点：目录遍历与文件面板同过滤（隐藏项 / `.tmp` / 团队排除名单）；
/// 日志只记模式与命中数，不记匹配行内容（可能含用户文本）。
pub async fn grep(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
    Json(body): Json<GrepBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let re = Regex::new(&body.pattern).map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("正则表达式无效：{e}")))?;
    let include_matcher = match body.include.as_deref() {
        Some(p) => Some(build_glob_matcher(p).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?),
        None => None,
    };
    let exclude = crate::meta::space_exclusions(&state, &space_id).await;
    let requested = body.path.clone();
    // 基准路径解析与逐文件扫描都是阻塞 IO（正则匹配也在其中），整段放进阻塞池
    let (root_dir, include_for_io) = (root.0.clone(), include_matcher);
    let (retained, total) = blocking("grep 扫描", move || {
        let (base_rel, base_is_file) =
            resolve_base(&SpaceRoot(root_dir.clone()), requested.as_deref())
                .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
        let mut retained: Vec<GrepMatchRow> = Vec::new();
        let mut total = 0usize;
        scan_for_matches(
            &root_dir,
            &base_rel,
            base_is_file,
            &re,
            include_for_io.as_ref(),
            &exclude,
            &mut retained,
            &mut total,
        )
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        Ok((retained, total))
    })
    .await?;
    let capped = total > retained.len();
    tracing::debug!(space_id = %space_id, pattern_len = body.pattern.len(), total, "grep 完成");
    Ok(Json(json!({ "matches": retained, "total": total, "capped": capped })))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// 注入的刷新替身：必定失败并累加调用次数（用来验证冷却窗口内不再重扫）。
    fn failing_refresher(
        calls: Arc<AtomicUsize>,
    ) -> Arc<dyn Fn(&FsPath, &[String], &mut WikiIndex, &mut TagIndex) -> Result<(), String> + Send + Sync> {
        Arc::new(move |_root, _exclude, _wiki, _tags| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("模拟刷新失败".to_string())
        })
    }

    /// 查询一次缓存：Ok = 保留下来的反链条目数，Err = 刷新失败且无可沿用的旧结果。
    async fn query(
        cache: &SpaceIndexCache,
        state: &ServerState,
        dir: &std::path::Path,
    ) -> Result<usize, ApiError> {
        cache.with_space(state, "space-1", dir, |index| index.wiki.files.len()).await
    }

    /// 放一份「上次成功」的索引：冷却只对成功过的空间生效，测试须先造出这份缓存。
    fn seed_successful_cache(cache: &SpaceIndexCache) {
        let mut caches = lock_recover(&cache.caches);
        caches.entry("space-1".to_string()).or_default().refreshed = true;
    }

    /// Default 与 new 必须同口径：派生 Default 会把冷却时长置零，失败后每次查询都重扫全仓。
    #[test]
    fn default_cache_keeps_failed_retry_ttl() {
        assert_eq!(SpaceIndexCache::default().retry_ttl, FAILED_RETRY_TTL);
        assert_eq!(SpaceIndexCache::new().retry_ttl, FAILED_RETRY_TTL);
    }

    /// 刷新失败后的冷却窗口内连续查询只尝试一次刷新；窗口过后允许重试。
    #[tokio::test]
    async fn index_refresh_failure_is_throttled_within_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let state = ServerState::open(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = SpaceIndexCache::with_refresher(
            failing_refresher(calls.clone()),
            std::time::Duration::from_millis(150),
        );
        // 先有成功缓存，失败才进冷却；没有成功缓存时不会冷却（见下一条用例）
        seed_successful_cache(&cache);
        query(&cache, &state, dir.path()).await.expect("有成功缓存时刷新失败应沿用旧结果");
        query(&cache, &state, dir.path()).await.expect("冷却窗口内应沿用旧结果");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "冷却窗口内不得重扫");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        query(&cache, &state, dir.path()).await.expect("TTL 过后应重新尝试刷新");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "TTL 过后应允许重试");
    }

    /// 缓存为空 + 刷新失败：必须回错误，不能拿空索引冒充「没有反链」。
    #[tokio::test]
    async fn index_refresh_failure_without_success_cache_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let state = ServerState::open(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = SpaceIndexCache::with_refresher(
            failing_refresher(calls.clone()),
            std::time::Duration::from_millis(150),
        );
        let err = query(&cache, &state, dir.path())
            .await
            .expect_err("没有成功缓存时刷新失败必须回错误");
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.1.contains("模拟刷新失败"), "错误原因应透出刷新失败本身：{}", err.1);
        // 没有旧结果可冷却：下次查询立即重试（空缓存不能进入 5s 静默期）
        let err = query(&cache, &state, dir.path()).await.expect_err("仍失败时继续回错误");
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "无成功缓存时不进冷却窗口");
        assert!(lock_recover(&cache.failures).is_empty(), "无成功缓存不得写冷却记录");
    }

    /// 排除名单变化 + 刷新失败：旧索引与旧名单继续对外服务（契约），冷却过后再按新名单重建。
    #[tokio::test]
    async fn index_refresh_failure_keeps_old_cache_and_stale_exclusions() {
        let dir = tempfile::tempdir().unwrap();
        let state = ServerState::open(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = SpaceIndexCache::with_refresher(
            failing_refresher(calls.clone()),
            std::time::Duration::from_millis(150),
        );
        // 先放一份「上次成功」的索引与名单
        {
            let mut caches = lock_recover(&cache.caches);
            let index = caches.entry("space-1".to_string()).or_default();
            index.refreshed = true;
            index.exclude_folders = vec!["旧排除夹".to_string()];
            index
                .wiki
                .files
                .insert("旧排除夹/笔记.md".to_string(), FileStamp { mtime_ms: 1, size: 1 });
            index
                .wiki
                .refs
                .insert("旧排除夹/笔记.md".to_string(), extract_refs("[[目标]]"));
        }
        // 名单变化（团队元数据里没有排除项 = 空名单）+ 刷新失败
        let kept = query(&cache, &state, dir.path())
            .await
            .expect("有过成功缓存时刷新失败应沿用旧结果");
        assert_eq!(kept, 1, "刷新失败必须保留旧索引（不能清空）");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "本次确实尝试过刷新");
        {
            let caches = lock_recover(&cache.caches);
            let index = caches.get("space-1").unwrap();
            assert_eq!(
                index.exclude_folders,
                vec!["旧排除夹".to_string()],
                "失败期间沿用旧名单（被排除目录的条目仍在，属已知契约）"
            );
        }
        // 冷却窗口内的第二次查询直接返回旧缓存，不再重扫
        let kept = query(&cache, &state, dir.path()).await.expect("冷却窗口内应沿用旧结果");
        assert_eq!(kept, 1, "冷却窗口内仍返回旧索引");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "冷却窗口内不得重扫");
    }

    // ===== 反链 / 链接提取 =====

    #[test]
    fn extract_refs_wiki_and_path_forms() {
        let refs = extract_refs("见 [[目标]] 与 [[别名页|别名]]，链接 [文本](笔记/目标.md)，图片 ![](x.png) 不算");
        assert_eq!(refs.len(), 3, "图片不算引用：{refs:?}");
        assert_eq!(refs[0].name.as_deref(), Some("目标"));
        assert_eq!(refs[1].name.as_deref(), Some("别名页"));
        assert_eq!(refs[2].path.as_deref(), Some("笔记/目标.md"));
    }

    #[test]
    fn normalize_link_path_rejects_traversal() {
        assert_eq!(normalize_link_path("./笔记/a.md"), Some("笔记/a.md".to_string()));
        assert_eq!(normalize_link_path("/abs/a.md"), Some("abs/a.md".to_string()));
        assert_eq!(normalize_link_path("a\\b.md"), Some("a/b.md".to_string()));
        assert_eq!(normalize_link_path("a%20b.md"), Some("a b.md".to_string()));
        assert_eq!(normalize_link_path("../x.md"), None);
        assert_eq!(normalize_link_path(""), None);
    }

    #[test]
    fn backlinks_basename_fallback_only_when_unique() {
        let mut index = WikiIndex::default();
        let content = "[[甲]] 与 [文本](目录/甲.md)";
        index.files.insert("笔记/乙.md".to_string(), FileStamp { mtime_ms: 1, size: 1 });
        index.refs.insert("笔记/乙.md".to_string(), extract_refs(content));
        // 目标「甲」按名字命中
        let rows = query_wiki_backlinks(&index, "甲", "笔记/甲.md");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file, "笔记/乙.md");
        // 路径形式在 basename 唯一时兜底命中
        let rows = query_wiki_backlinks(&index, "甲", "其他/甲.md");
        assert_eq!(rows.len(), 1);
        // 同名 basename 不唯一时只认精确路径：再添一个同 basename 文件（笔记/甲.md）
        index
            .files
            .insert("笔记2/甲.md".to_string(), FileStamp { mtime_ms: 1, size: 1 });
        index
            .refs
            .insert("笔记2/甲.md".to_string(), extract_refs("[[甲]]"));
        index
            .files
            .insert("笔记/甲.md".to_string(), FileStamp { mtime_ms: 1, size: 1 });
        index
            .refs
            .insert("笔记/甲.md".to_string(), extract_refs("[[甲]]"));
        let rows = query_wiki_backlinks(&index, "乙名", "另处/甲.md");
        assert!(
            rows.iter().all(|r| r.file != "笔记/乙.md"),
            "basename 不唯一时路径式链接不得兜底命中：{rows:?}"
        );
    }

    // ===== 标签提取 =====

    #[test]
    fn tags_from_frontmatter_and_inline_skip_code() {
        let content = "---\ntags: [工作, \"休息\"]\n---\n标记 #生活 在正文\n```rust\n#围栏内不算\n```\n    #缩进代码不算\n行内 `#行内代码不算` 不算\n#2024 纯数字不算\nfoo#bar 前缀字母不算\nhttps://x.com/#faq URL 片段不算";
        let tags = extract_note_tags(content);
        assert!(tags.contains("工作") && tags.contains("休息") && tags.contains("生活"), "{tags:?}");
        for absent in ["围栏内不算", "缩进代码不算", "行内代码不算", "2024", "bar", "faq"] {
            assert!(!tags.contains(absent), "{absent} 不应入索引：{tags:?}");
        }
    }

    #[test]
    fn inline_tag_requires_at_least_one_letter() {
        let tags = extract_note_tags("#标签-1 与 #EN_2 与 #1");
        assert!(tags.contains("标签-1") && tags.contains("EN_2"), "{tags:?}");
        assert!(!tags.contains("1"));
    }

    /// 与客户端 `vault::extract_note_tags` 同口径：只认列首顶层键，值里的注释与跨行数组都要认。
    #[test]
    fn frontmatter_tags_edge_shapes() {
        // 值位只有注释：不是标签，也不造出 `# 备注`
        assert!(extract_note_tags("---\ntags: # 备注\n---\n").is_empty());
        // 值位只有注释 + 后续块列表
        let tags = extract_note_tags("---\ntags: # 备注\n  - a\n  - b\n---\n");
        assert!(tags.contains("a") && tags.contains("b"), "{tags:?}");
        // 缩进嵌套键与 block scalar 文本里的 `tags:` 不算；整块统一缩进的顶层键仍算
        assert!(extract_note_tags("---\nmeta:\n  tags: [x]\n---\n").is_empty());
        assert!(extract_note_tags("---\n  tags: [a]\n---\n").contains("a"));
        assert!(extract_note_tags("---\nnote: |\n  tags: [y]\n---\n").is_empty());
        // 未闭合的流式数组整体忽略
        assert!(extract_note_tags("---\ntags: [a, b\n---\n").is_empty());
        // 跨行流式数组 + 行内注释
        let tags = extract_note_tags("---\ntags: [\n  a, # 一\n  b\n]\n---\n");
        assert!(tags.contains("a") && tags.contains("b"), "{tags:?}");
    }

    #[test]
    fn aggregate_tags_keeps_every_tag_sorted_by_count() {
        let mut index = TagIndex::default();
        for i in 0..1004 {
            index.tags.insert(format!("f{i}.md"), HashSet::from([format!("t{i:04}")]));
        }
        index.tags.insert("a.md".to_string(), HashSet::from(["hot".to_string(), "t0000".to_string()]));
        index.tags.insert("b.md".to_string(), HashSet::from(["hot".to_string()]));
        let rows = aggregate_tag_counts(&index);
        assert_eq!(rows.len(), 1005, "不截断");
        assert_eq!(rows[0].tag, "hot");
        assert_eq!(rows[0].count, 2);
    }

    /// 两端共用 `fixtures/tags/parse.json`：`cases` 是常规形态，`alignedCases` 是
    /// frontmatter 定界边界（BOM、非规范分隔行），期望值取自客户端；两端必须切在同一位置。
    /// 任一形态两端提取结果都必须与 tags 完全一致（顺序无关）。
    #[test]
    fn tag_parse_fixture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/tags/parse.json");
        let raw = std::fs::read_to_string(path)
            .expect("夹具缺失：fixtures/tags/parse.json 应随源码入库");
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

    // ===== glob / grep =====

    #[test]
    fn glob_without_slash_matches_any_depth_with_literal_separator() {
        let m = build_glob_matcher("*.ts").unwrap();
        assert!(m.is_match("a.ts") && m.is_match("src/deep/b.ts"));
        assert!(!m.is_match("src/a.js"));
        let anchored = build_glob_matcher("src/*.ts").unwrap();
        assert!(anchored.is_match("src/a.ts"));
        assert!(!anchored.is_match("src/deep/b.ts"), "literal_separator 下单星不跨层");
    }

    #[test]
    fn normalize_base_path_collapses_slashes() {
        assert_eq!(normalize_base_path("notes/"), "notes");
        assert_eq!(normalize_base_path("./notes"), "notes");
        assert_eq!(normalize_base_path("a//b\\"), "a/b");
        assert_eq!(normalize_base_path("."), "");
        assert_eq!(normalize_base_path("/"), "");
    }

    #[test]
    fn scan_content_counts_all_and_respects_caps() {
        let re = Regex::new("hit").unwrap();
        let content = "hit 1\nmiss\nhit 2\n";
        let mut retained = Vec::new();
        let mut total = 0usize;
        scan_content(content, "a.md", &re, &mut retained, &mut total);
        assert_eq!(total, 2);
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].line_number, 1);
        assert_eq!(retained[1].line_number, 3);
    }

    #[test]
    fn preview_line_truncates_on_char_boundary() {
        let long = format!("汉{}", "字".repeat(2000));
        let preview = preview_line(&long, 2000);
        assert!(preview.ends_with("... (line truncated)"));
        assert!(preview.is_char_boundary(preview.len() - "... (line truncated)".len()));
        assert_eq!(preview_line("short", 2000), "short");
    }

    #[test]
    fn is_binary_probes_leading_bytes() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text only"));
    }

    #[test]
    fn percent_decode_keeps_invalid_sequences() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
    }
}
