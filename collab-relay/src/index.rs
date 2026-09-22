//! 派生索引与扫描（真源 = 空间内容文件树）：反链 / 标签 / glob / grep。
//!
//! 与客户端检索同一套语义与同一批引擎（globset + regex）：
//! - 反链 / 标签索引：纯内存只读派生，按文件指纹（mtime 毫秒 + 大小）增量刷新，
//!   消失文件剔除——内容在服务器上被本服务写入，无外部编辑者，指纹刷新主要覆盖
//!   重命名 / 删除后的索引收敛。查询频率低，刷新是 stat 遍历（快），不做后台预热。
//! - glob / grep：目录遍历与客户端文件面板同过滤（隐藏项 / `.tmp`），命中上限与字节预算一致。
//!
//! 日志只记模式与命中数，不记匹配行内容（可能含用户文本）。

use std::collections::{HashMap, HashSet};
use std::path::Path as FsPath;
use std::sync::OnceLock;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use globset::{GlobBuilder, GlobMatcher};
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::content::member_root;
use crate::fsops::{file_mtime_secs, read_dir_filtered, SpaceRoot};
use crate::state::ServerState;
use crate::{ApiError, ApiResult};

// ===== 索引缓存（进程内，随空间内容重建，不持久化） =====

/// 单空间的派生索引（反链 + 标签）。`exclude_folders` 记录构建本缓存时的团队排除名单：
/// 名单变化时整份作废重建（否则被排除目录的陈旧条目会残留在索引里）。
#[derive(Default)]
pub struct SpaceIndex {
    wiki: WikiIndex,
    tags: TagIndex,
    exclude_folders: Vec<String>,
}

// ===== 反链索引 =====

/// 反链行：引用方笔记的相对路径 + 标题（对应前端 types/canvas.ts）。
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
#[derive(PartialEq)]
struct FileStamp {
    mtime_ms: u128,
    size: u64,
}

/// 反链索引：rel 路径 → 指纹 + 已提取引用（倒排查询时遍历，纯内存迭代无 I/O）。
#[derive(Default)]
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

/// 递归遍历空间 .md（与文件树同过滤：跳过隐藏目录与团队排除文件夹）。
fn walk_md_in(
    root: &FsPath,
    rel: &str,
    exclude_folders: &[String],
    f: &mut dyn FnMut(&str, &FsPath) -> Result<(), String>,
) -> Result<(), String> {
    let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    if !dir.exists() {
        return Ok(());
    }
    for (child_rel, is_dir) in read_dir_filtered(&dir, rel, exclude_folders)? {
        if is_dir {
            walk_md_in(root, &child_rel, exclude_folders, f)?;
        } else if child_rel.ends_with(".md") {
            f(&child_rel, &root.join(&child_rel))?;
        }
    }
    Ok(())
}

fn file_stamp(path: &FsPath) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    Some(FileStamp { mtime_ms, size: meta.len() })
}

// ===== 标签索引 =====

/// 标签行：标签名 + 全空间出现次数（对应前端 types/tags.ts）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagRow {
    pub tag: String,
    pub count: u32,
}

/// 标签索引：rel 路径 → 指纹 + 标签集合。
#[derive(Default)]
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

/// 聚合标签计数：count 降序 + 名称升序，上限 1000。
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
    rows.truncate(1000);
    rows
}

/// 提取一篇笔记的全部标签：frontmatter `tags` + 正文内联 `#标签`（去重）。
/// 标签须含 ≥1 个字母（排除 `#2024` 日期序号误判）；正文提取跳过 frontmatter /
/// 围栏代码（含 ≤3 空格缩进）/ 缩进代码（≥4 空格行首）/ 行内代码。
fn extract_note_tags(content: &str) -> HashSet<String> {
    let mut tags: HashSet<String> = HashSet::new();
    let mut body_start = 0;
    // 仅文档开头 `---` 块内的 `tags` 键算 frontmatter（非开头 `---` 是正文横隔条，不算）
    if content.starts_with("---\n") || content.starts_with("---\r\n") {
        if let Some(end) = frontmatter_end(content) {
            parse_fm_tags(&content[..end], &mut tags);
            body_start = end;
        }
    }
    extract_inline_tags(&content[body_start..], &mut tags);
    tags
}

/// 解析 frontmatter 块的 `tags` 键：`tags: [a, b]` 内联数组、`tags:\n  - a` 块列表、
/// `tags: foo` 标量，去引号去空。块列表项正则要求 `-` 后跟空白（防 `---` 文档结束符被当成列表项）。
fn parse_fm_tags(fm: &str, tags: &mut HashSet<String>) {
    static INLINE_RE: OnceLock<Regex> = OnceLock::new();
    static KEY_LINE_RE: OnceLock<Regex> = OnceLock::new();
    static ITEM_RE: OnceLock<Regex> = OnceLock::new();
    static SCALAR_RE: OnceLock<Regex> = OnceLock::new();
    let inline = INLINE_RE.get_or_init(|| Regex::new(r"(?m)^\s*tags\s*:\s*\[([^\]]*)\]").unwrap());
    let key_line = KEY_LINE_RE.get_or_init(|| Regex::new(r"(?m)^\s*tags\s*:\s*$").unwrap());
    let item = ITEM_RE.get_or_init(|| Regex::new(r"^\s*-\s+(.+?)\s*$").unwrap());
    let scalar = SCALAR_RE.get_or_init(|| Regex::new(r"(?m)^\s*tags\s*:\s*(.+?)\s*$").unwrap());
    // 内联数组优先（`tags: [a, b]`；括号捕获天然排除尾部注释）
    if let Some(caps) = inline.captures(fm) {
        if let Some(group) = caps.get(1) {
            for raw in group.as_str().split(',') {
                let t = clean_tag_value(raw);
                if !t.is_empty() {
                    tags.insert(t.to_string());
                }
            }
        }
        return;
    }
    // 块列表：`tags:` 行后连续 `- x` 行（空行跳过，首个非空非列表行结束该块）
    if let Some(m) = key_line.find(fm) {
        for line in fm[m.end()..].lines() {
            if line.trim().is_empty() {
                continue;
            }
            match item.captures(line) {
                Some(caps) => {
                    let t = clean_tag_value(caps.get(1).unwrap().as_str());
                    if !t.is_empty() {
                        tags.insert(t.to_string());
                    }
                }
                None => break,
            }
        }
        return;
    }
    // 标量（`tags: foo`；剥离 ` # 注释`）
    if let Some(caps) = scalar.captures(fm) {
        if let Some(group) = caps.get(1) {
            let t = clean_tag_value(group.as_str());
            if !t.is_empty() {
                tags.insert(t.to_string());
            }
        }
    }
}

/// 剥离 YAML 行内注释（` # ...`，`foo#bar` 无空格前缀不算注释）与引号。
fn clean_tag_value(raw: &str) -> &str {
    raw.split(" #").next().unwrap_or(raw).trim().trim_matches(['"', '\''])
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

/// 简易 percent 解码（%XX）；非法序列原样保留（匹配不上自然不命中，不报错）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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

/// 文档开头 frontmatter 结束位置（`---\n ... ---\n` 闭合行之后）；无闭合返回 None。
fn frontmatter_end(content: &str) -> Option<usize> {
    let first_nl = content.find('\n')?;
    let mut pos = first_nl + 1;
    for line in content[first_nl + 1..].split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some(start + line.len());
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

/// 取（或懒建）空间的派生索引并增量刷新。刷新是 stat 遍历 + 变化文件重读；
/// 缓存为全局单锁（所有空间串行刷新），且持锁做阻塞文件 I/O——目标规模（≤30 人内容量）
/// 下延迟可忽略，不值得为此引入分空间锁或后台任务。
/// 团队排除名单变化时整份缓存作废重建（成员改了排除夹后索引立即收敛，不留陈旧条目）。
fn with_space_index<R>(
    state: &ServerState,
    space_id: &str,
    root: &FsPath,
    f: impl FnOnce(&mut SpaceIndex) -> R,
) -> R {
    let exclude = crate::meta::space_exclusions(state.data_dir(), space_id);
    let mut caches = state.inner_index_cache().lock().unwrap();
    let index = caches.entry(space_id.to_string()).or_default();
    if index.exclude_folders != exclude {
        *index = SpaceIndex { exclude_folders: exclude.clone(), ..Default::default() };
    }
    if let Err(e) = refresh_wiki_index(root, &exclude, &mut index.wiki) {
        tracing::warn!(space_id = %space_id, "反链索引刷新失败：{e}");
    }
    if let Err(e) = refresh_tag_index(root, &exclude, &mut index.tags) {
        tracing::warn!(space_id = %space_id, "标签索引刷新失败：{e}");
    }
    f(index)
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
    let rows = with_space_index(&state, &space_id, &root.0, |index| {
        query_wiki_backlinks(&index.wiki, &query.note_name, &query.note_file)
    });
    Ok(Json(serde_json::to_value(rows).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?))
}

pub async fn tags(
    State(state): State<ServerState>,
    user: AuthUser,
    Path(space_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let rows = with_space_index(&state, &space_id, &root.0, |index| aggregate_tag_counts(&index.tags));
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
    let (base_rel, base_is_file) = resolve_base(&root, body.path.as_deref()).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    let exclude = crate::meta::space_exclusions(state.data_dir(), &space_id);
    let mut entries = collect_glob_files(&root.0, &base_rel, base_is_file, &matcher, &exclude)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
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
    let (base_rel, base_is_file) = resolve_base(&root, body.path.as_deref()).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    let exclude = crate::meta::space_exclusions(state.data_dir(), &space_id);
    let mut retained: Vec<GrepMatchRow> = Vec::new();
    let mut total = 0usize;
    scan_for_matches(&root.0, &base_rel, base_is_file, &re, include_matcher.as_ref(), &exclude, &mut retained, &mut total)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let capped = total > retained.len();
    tracing::debug!(space_id = %space_id, pattern_len = body.pattern.len(), total, "grep 完成");
    Ok(Json(json!({ "matches": retained, "total": total, "capped": capped })))
}

#[cfg(test)]
mod tests {
    use super::*;

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
