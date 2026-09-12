//! 仓库内文件检索命令（glob / grep 两个 AI 工具的 Rust 后端）。
//!
//! - `glob_vault`：按 glob 模式枚举仓库内文件路径（只返回文件、不含目录），按修改时间升序，
//!   最多内联返回 `GLOB_MAX_RESULTS` 条，超限附精确总数供前端提示收窄。
//! - `grep_vault`：用正则搜索仓库内文件内容，返回匹配行（绝对行号 + 路径，前端按文件分组），
//!   最多内联返回 `GREP_MAX_MATCHES` 条，超限附精确总数；单行预览按字节截断且保持 UTF-8 边界。
//!
//! 目录遍历复用 `vault::read_dir_filtered`，与文件面板语义一致：跳过隐藏 `.` 开头目录（含
//! `.atelyx` 内部侧文件）、排除文件夹与 `.tmp` 原子写副产物。glob 匹配用 globset、内容搜索
//! 用 regex（各自领域的事实标准实现），模式语义对模型透明：不含「/」的 glob 自动前缀 `**/`
//! （匹配任意深度文件名），正则无效/glob 无效直接报错回填由模型自纠。
//! 安全边界 = 仓库根：`path` 参数经 `vault::safe_join` 校验（拒绝绝对路径 / `..` / 越界）。

use std::path::Path;

use regex::Regex;
use serde::Serialize;
use tauri::State;

use crate::vault::{has_hidden_segment, read_dir_filtered, safe_join, VaultState};

/// glob 单次内联返回的路径上限（超限附 total，前端提示收窄）。
const GLOB_MAX_RESULTS: usize = 100;
/// grep 单次内联返回的匹配上限（超限附 total，前端提示收窄）。
const GREP_MAX_MATCHES: usize = 250;
/// 单行预览的字节上限（截断保持 UTF-8 字符边界）。
const GREP_MAX_LINE_BYTES: usize = 2000;
/// 二进制探测窗口：前 N 字节含 `\0` 判二进制跳过（与常规文本/二进制判别一致）。
const GREP_BINARY_PROBE_BYTES: usize = 8192;
/// 单个文件的最大读取字节数：超大文件跳过（防病态输入拖慢整轮检索；普通文本文件远小于此）。
const GREP_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// grep 结果回填的聚合字节预算：单行截断 2000B × 250 条最坏 ~500KB，会撑爆模型上下文；
/// 与 read_file 分页「不整文件回填」同一口径，超限停止保留（total 仍精确计数，前端据此提示收窄）。
const GREP_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// `glob_vault` 返回结果（paths 相对仓库根、`/` 分隔，与前端 read_file/edit_file 同坐标）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobVaultResult {
    /// 搜索基准：path 参数原样（缺省 = 空串 = 仓库根）。
    pub root: String,
    /// 命中的文件路径（按修改时间升序，最多 GLOB_MAX_RESULTS 条）。
    pub paths: Vec<String>,
    /// 全部命中数（可能大于 paths.len()，超限时用于提示收窄）。
    pub total: usize,
    /// 是否因超上限被截断（total > paths.len()）。
    pub capped: bool,
}

/// grep 匹配行（path 相对仓库根、`/` 分隔）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepMatchRow {
    pub path: String,
    /// 文件内 1-based 行号。
    pub line_number: usize,
    /// 行内容（超长已按 GREP_MAX_LINE_BYTES 截断并附后缀）。
    pub line: String,
}

/// `grep_vault` 返回结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepVaultResult {
    /// 内联返回的匹配行（最多 GREP_MAX_MATCHES 条，行号升序、按文件连续）。
    pub matches: Vec<GrepMatchRow>,
    /// 全部匹配数（可能大于 matches.len()，超限时用于提示收窄）。
    pub total: usize,
    /// 是否因超上限被截断（total > matches.len()）。
    pub capped: bool,
}

/// 构建 glob 匹配器：模式不含「/」时自动前缀 `**/`，使 `*.ts` 匹配任意深度的文件名
/// （与「模式含「/」才锚定层级」的描述语义一致；`**` 可匹配零个或多个路径段）。
/// `literal_separator(true)`：`*`/`?`/字符类不跨 `/`（`src/*.ts` 不命中 `src/deep/b.ts`），
/// 仅 `**` 可跨层——与 rg 的 --glob 语义一致。
fn build_glob_matcher(pattern: &str) -> Result<globset::GlobMatcher, String> {
    let normalized = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    };
    let glob = globset::GlobBuilder::new(&normalized)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("glob 模式无效：{e}"))?;
    Ok(glob.compile_matcher())
}

/// 归一化 path 参数为规范相对路径（`\` → `/`、去尾斜杠、去 `./` 前缀、折叠中间 `//`；空/`.` = 仓库根）。
/// 防模型传入 `notes/` / `./notes` 时 walk 拼出 `notes//a.md` 双斜杠 rel，导致锚定 glob
/// （`notes/*.md`）全串失配返回空结果——AI 传目录带尾斜杠/`./` 前缀是常见形态。
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

/// 解析 `path` 参数：缺省 = 仓库根（目录）；给出 = 归一化 + `safe_join` 校验后按「文件/目录」区分。
/// 指向隐藏目录（. 开头段）拒绝（AI 发现层完全屏蔽）。返回（相对仓库根的展示前缀，是否单文件）。
fn resolve_base(root: &Path, path: Option<&str>) -> Result<(String, bool), String> {
    let p = match path {
        None => return Ok((String::new(), false)),
        Some(p) => normalize_base_path(p),
    };
    if p.is_empty() {
        // 归一化后为空（`"/"`/`"./"`/`"."`）→ 视作仓库根
        return Ok((String::new(), false));
    }
    if has_hidden_segment(&p) {
        return Err("路径位于隐藏目录（. 开头段），AI 工具不可访问".to_string());
    }
    let abs = safe_join(root, &p, false)?;
    let meta = std::fs::metadata(&abs).map_err(|_| format!("路径不存在或不可访问：{p}"))?;
    Ok((p, meta.is_file()))
}

/// 文件 mtime unix 秒（失败回退 0，与文件树 `list_tree_in` 同一取法——单文件失败不拖垮整轮）。
fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|md| md.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 递归收集目录树内全部文件（相对路径 + mtime），过滤规则走 `read_dir_filtered`。
fn walk_files(root: &Path, rel: &str, exclude: &[String], out: &mut Vec<(String, i64)>) -> Result<(), String> {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let mut entries = read_dir_filtered(&dir, rel, exclude)?;
    // 按名排序保证遍历顺序确定（read_dir 顺序由文件系统决定）
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (child_rel, is_dir) in entries {
        if is_dir {
            walk_files(root, &child_rel, exclude, out)?;
        } else {
            let mtime = file_mtime(&root.join(&child_rel));
            out.push((child_rel, mtime));
        }
    }
    Ok(())
}

/// 按 glob 模式收集命中文件（单文件基准时只判定该文件）。
fn collect_glob_files(
    root: &Path,
    base_rel: &str,
    base_is_file: bool,
    exclude: &[String],
    matcher: &globset::GlobMatcher,
) -> Result<Vec<(String, i64)>, String> {
    if base_is_file {
        let mut out = Vec::new();
        if matcher.is_match(base_rel) {
            out.push((base_rel.to_string(), file_mtime(&root.join(base_rel))));
        }
        return Ok(out);
    }
    let mut out = Vec::new();
    walk_files(root, base_rel, exclude, &mut out)?;
    out.retain(|(rel, _)| matcher.is_match(rel));
    Ok(out)
}

/// 按 glob 模式枚举仓库内文件路径（相对仓库根、`/` 分隔；只返回文件；跳过隐藏目录/排除文件夹）。
/// 结果按修改时间升序（`rg --sort=modified` 同语义），最多内联返回 GLOB_MAX_RESULTS 条。
/// 目录遍历是纯阻塞 IO：放 `spawn_blocking`，不占 async 执行器（否则并发 glob/grep 会互相排队）。
#[tauri::command]
pub async fn glob_vault(
    pattern: String,
    path: Option<String>,
    state: State<'_, VaultState>,
) -> Result<GlobVaultResult, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    tauri::async_runtime::spawn_blocking(move || {
        let matcher = build_glob_matcher(&pattern)?;
        let (base_rel, base_is_file) = resolve_base(&root, path.as_deref())?;
        let mut entries = collect_glob_files(&root, &base_rel, base_is_file, &exclude, &matcher)?;
        entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let total = entries.len();
        let capped = total > GLOB_MAX_RESULTS;
        let paths: Vec<String> = entries
            .into_iter()
            .take(GLOB_MAX_RESULTS)
            .map(|(rel, _)| rel)
            .collect();
        Ok(GlobVaultResult {
            root: path.unwrap_or_default(),
            paths,
            total,
            capped,
        })
    })
    .await
    .map_err(|e| format!("检索线程失败：{e}"))?
}

/// 前 N 字节含 `\0` 判二进制（与常规文本/二进制判别一致，跳过不可读内容避免垃圾回填）。
fn is_binary(bytes: &[u8]) -> bool {
    let probe_len = bytes.len().min(GREP_BINARY_PROBE_BYTES);
    bytes[..probe_len].contains(&0)
}

/// 按字节上限截断单行预览，落在 UTF-8 字符边界（截断处不产生乱码），超长附后缀提示。
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
/// 保留前 GREP_MAX_MATCHES 条且受聚合字节预算 max_output_bytes 约束、计数全部命中。
/// 命令与单测共用。
fn scan_content(
    content: &str,
    rel: &str,
    re: &Regex,
    retained: &mut Vec<GrepMatchRow>,
    total: &mut usize,
    max_output_bytes: usize,
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
        // 聚合字节预算：超限停止保留（total 仍精确计数），capped 由调用方按 total > retained.len() 判定
        if out_bytes + preview.len() > max_output_bytes {
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
fn scan_file(root: &Path, rel: &str, re: &Regex, retained: &mut Vec<GrepMatchRow>, total: &mut usize) {
    let abs = root.join(rel);
    let meta = match std::fs::metadata(&abs) {
        Ok(m) => m,
        Err(_) => return, // 读取失败（权限/竞态删除等）跳过该文件，不拖垮整轮
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
    // 非 UTF-8 文本按替换字符容错（与 read_vault_file 同一口径）
    let content = String::from_utf8_lossy(&bytes);
    scan_content(&content, rel, re, retained, total, GREP_MAX_OUTPUT_BYTES);
}

/// 递归搜索目录树内文件（可选 include 正向 glob 过滤），单文件基准时只搜该文件。
/// 目录遍历复用 walk_files（与 glob 同一套过滤/排序，顺序确定），避免两套遍历实现。
fn scan_for_matches(
    root: &Path,
    base_rel: &str,
    is_file: bool,
    exclude: &[String],
    re: &Regex,
    include: Option<&globset::GlobMatcher>,
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
    walk_files(root, base_rel, exclude, &mut files)?;
    for (rel, _) in files {
        if include.map(|m| m.is_match(&rel)).unwrap_or(true) {
            scan_file(root, &rel, re, retained, total);
        }
    }
    Ok(())
}

/// 用正则搜索仓库内文件内容（相对仓库根路径，与 read_file 同坐标），返回匹配行
/// （1-based 绝对行号；单行预览按 GREP_MAX_LINE_BYTES 字节截断），最多内联返回
/// GREP_MAX_MATCHES 条、恒附精确总数。二进制/超大/不可读文件跳过。
/// 全仓库扫描是纯阻塞 IO：放 `spawn_blocking`，不占 async 执行器。
#[tauri::command]
pub async fn grep_vault(
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    state: State<'_, VaultState>,
) -> Result<GrepVaultResult, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    tauri::async_runtime::spawn_blocking(move || {
        let re = Regex::new(&pattern).map_err(|e| format!("正则表达式无效：{e}"))?;
        let include_matcher = match include.as_deref() {
            Some(p) => Some(build_glob_matcher(p)?),
            None => None,
        };
        let (base_rel, base_is_file) = resolve_base(&root, path.as_deref())?;
        let mut retained: Vec<GrepMatchRow> = Vec::new();
        let mut total = 0usize;
        scan_for_matches(
            &root,
            &base_rel,
            base_is_file,
            &exclude,
            &re,
            include_matcher.as_ref(),
            &mut retained,
            &mut total,
        )?;
        let capped = total > retained.len();
        Ok(GrepVaultResult {
            matches: retained,
            total,
            capped,
        })
    })
    .await
    .map_err(|e| format!("检索线程失败：{e}"))?
}

#[cfg(test)]
mod filesearch_tests {
    use super::*;

    #[test]
    fn glob_without_slash_matches_at_any_depth() {
        let m = build_glob_matcher("*.ts").unwrap();
        assert!(m.is_match("a.ts"));
        assert!(m.is_match("src/a.ts"));
        assert!(m.is_match("src/deep/b.ts"));
        assert!(!m.is_match("src/a.js"));
    }

    #[test]
    fn glob_with_slash_anchors_depth() {
        let m = build_glob_matcher("src/*.ts").unwrap();
        assert!(m.is_match("src/a.ts"));
        assert!(!m.is_match("a.ts"));
        assert!(!m.is_match("src/deep/b.ts"));
    }

    #[test]
    fn glob_double_star_any_depth() {
        let m = build_glob_matcher("**/*.md").unwrap();
        assert!(m.is_match("a.md"));
        assert!(m.is_match("x/y/z.md"));
        assert!(!m.is_match("x/y/z.txt"));
    }

    #[test]
    fn glob_invalid_pattern_rejected() {
        assert!(build_glob_matcher("[").is_err());
    }

    #[test]
    fn preview_line_short_passthrough() {
        assert_eq!(preview_line("hello", 2000), "hello");
    }

    #[test]
    fn preview_line_truncates_ascii() {
        let long = "a".repeat(3000);
        let out = preview_line(&long, 2000);
        assert!(out.starts_with(&"a".repeat(2000)));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn preview_line_keeps_utf8_boundary() {
        // 「中」= 3 字节：2000 不是字符边界，应回退到 1998（666 字符整）
        let long = "中".repeat(1000);
        let out = preview_line(&long, 2000);
        let prefix = out.split("...").next().unwrap();
        assert!(prefix.len() <= 2000);
        assert!(prefix.ends_with('中'));
    }

    #[test]
    fn binary_probe_detects_nul() {
        assert!(is_binary(&[0u8; 100]));
        assert!(!is_binary(b"hello world\n"));
        let mut mixed = b"abc".to_vec();
        mixed.push(0);
        assert!(is_binary(&mixed));
    }

    #[test]
    fn normalize_base_path_variants() {
        // 尾斜杠 / `./` 前缀 / 反斜杠 / 中间双斜杠 → 规范相对路径；空/`.`/`/` → 仓库根
        assert_eq!(normalize_base_path("notes/"), "notes");
        assert_eq!(normalize_base_path("./notes"), "notes");
        assert_eq!(normalize_base_path("./notes/"), "notes");
        assert_eq!(normalize_base_path("a//b"), "a/b");
        assert_eq!(normalize_base_path("a\\b"), "a/b");
        assert_eq!(normalize_base_path("notes"), "notes");
        assert_eq!(normalize_base_path("./"), "");
        assert_eq!(normalize_base_path("."), "");
        assert_eq!(normalize_base_path("/"), "");
        // `..` 不归一化（留给 safe_join 的穿越校验拒绝）
        assert_eq!(normalize_base_path("../x"), "../x");
    }

    #[test]
    fn resolve_base_rejects_hidden_path() {
        // 显式指向隐藏目录 → 拒绝（在触碰文件系统前即返回 Err）
        assert!(resolve_base(Path::new("C:/"), Some(".atelyx")).is_err());
        assert!(resolve_base(Path::new("C:/"), Some("a/.hidden")).is_err());
        assert!(resolve_base(Path::new("C:/"), Some(".gitignore")).is_err());
        // 缺省/空 = 仓库根，正常
        assert!(resolve_base(Path::new("C:/"), None).is_ok());
        assert!(resolve_base(Path::new("C:/"), Some("")).is_ok());
    }

    #[test]
    fn scan_counts_all_and_retains_cap() {
        let mut content = String::new();
        for i in 1..=300 {
            content.push_str(&format!("match {i}\n"));
        }
        content.push_str("plain\n");
        let re = Regex::new("match").unwrap();
        let mut retained = Vec::new();
        let mut total = 0;
        scan_content(&content, "f.txt", &re, &mut retained, &mut total, usize::MAX);
        assert_eq!(total, 300);
        assert_eq!(retained.len(), GREP_MAX_MATCHES);
        assert_eq!(retained[0].line_number, 1);
        assert_eq!(retained.last().unwrap().line_number, GREP_MAX_MATCHES);
    }

    #[test]
    fn scan_strips_carriage_return_and_numbers_lines() {
        let re = Regex::new("b").unwrap();
        let mut retained = Vec::new();
        let mut total = 0;
        scan_content("a\r\nb\r\nc", "f.txt", &re, &mut retained, &mut total, usize::MAX);
        assert_eq!(total, 1);
        assert_eq!(retained[0].line_number, 2);
        assert_eq!(retained[0].line, "b");
    }

    #[test]
    fn scan_output_byte_budget_stops_retention() {
        // 每行 3000 字符 → 预览截断为 2000 字节 + 截断后缀（约 2020 字节/条）；
        // 预算 5000 → 保留前 2 条（2020×2 ≤ 5000，第 3 条 6060 > 5000），total 仍精确
        let content = vec!["x".repeat(3000); 5].join("\n");
        let re = Regex::new("x").unwrap();
        let mut retained = Vec::new();
        let mut total = 0;
        scan_content(&content, "f.txt", &re, &mut retained, &mut total, 5000);
        assert_eq!(total, 5);
        assert_eq!(retained.len(), 2);
    }
}
