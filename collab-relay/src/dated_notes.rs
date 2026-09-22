//! 带日期笔记扫描（主页日历）：扫空间 `.md` 的 frontmatter `date`/`due` 字段。
//!
//! 与客户端本地 Rust `commands/home.rs::list_dated_notes` 同一套解析口径（frontmatter 块识别、
//! `YYYY-MM-DD` 提取、只读文件头、尊重排除文件夹），只是真相源改为服务端内容树。
//! 只读、尽力而为：读失败/无日期/超上限一律跳过，不阻塞面板。

use std::path::Path;
use std::sync::OnceLock;

use axum::extract::{Path as AxumPath, State};
use axum::Json;
use regex::Regex;
use serde::Serialize;
use serde_json::json;

use crate::auth::AuthUser;
use crate::content::member_root;
use crate::fsops::read_dir_filtered;
use crate::state::ServerState;
use crate::ApiResult;

/// 带日期笔记扫描上限（防御性；超过即截断，日历仍有手动日程兜底）。
const DATED_NOTE_CAP: usize = 2000;
/// 单个 .md 读取字节上限（frontmatter 解析只需文件头）。
const MD_HEAD_CAP: usize = 8192;

/// 带日期笔记（frontmatter `date`/`due`，自动进日历；值为 `YYYY-MM-DD`）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatedNote {
    file: String,
    title: String,
    date: Option<String>,
    due: Option<String>,
}

fn date_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*date\s*:\s*(.+)$").unwrap())
}

fn due_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*due\s*:\s*(.+)$").unwrap())
}

/// 取文件头（最多 max 字节，非 UTF-8 替换字符容错；剥离 UTF-8 BOM 使带 BOM 的 frontmatter 可识别）。
fn read_head(path: &Path, max: usize) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let head = &data[..data.len().min(max)];
    Some(String::from_utf8_lossy(head).trim_start_matches('\u{feff}').to_string())
}

/// 取 frontmatter 块（`---\n...\n---` 或 `...\n` 结尾；无块返回 None）。
fn frontmatter_block(head: &str) -> Option<&str> {
    let rest = head
        .strip_prefix("---\r\n")
        .or_else(|| head.strip_prefix("---\n"))?;
    let end = rest.find("\n---").or_else(|| rest.find("\n..."))?;
    Some(&rest[..end])
}

/// 取 frontmatter 块的 date/due 原始值（去引号）。
fn frontmatter_date_values(fm: &str) -> (Option<String>, Option<String>) {
    let pick = |caps: Option<regex::Captures<'_>>| -> Option<String> {
        caps.and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().trim_matches('"').to_string())
    };
    let date = pick(date_re().captures(fm));
    let due = pick(due_re().captures(fm));
    (date, due)
}

/// 从值中提取第一个 `YYYY-MM-DD`（值可为 `2024-01-15` / 带时间 / ISO）。
fn extract_ymd(value: &str) -> Option<String> {
    let b = value.as_bytes();
    let n = b.len();
    if n < 10 {
        return None;
    }
    let dig = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_digit());
    for i in 0..=n - 10 {
        if dig(i)
            && dig(i + 1)
            && dig(i + 2)
            && dig(i + 3)
            && b[i + 4] == b'-'
            && dig(i + 5)
            && dig(i + 6)
            && b[i + 7] == b'-'
            && dig(i + 8)
            && dig(i + 9)
        {
            return Some(value[i..i + 10].to_string());
        }
    }
    None
}

/// 递归遍历空间 `.md`（与文件树同过滤：隐藏目录 + 团队排除文件夹）。
fn walk_md_in(
    root: &Path,
    rel: &str,
    exclude_folders: &[String],
    f: &mut dyn FnMut(&str, &Path) -> Result<(), String>,
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

/// `GET /api/spaces/{space_id}/dated-notes` — 扫描带日期笔记（只读、尽力而为）。
/// 扫描是纯阻塞 IO，与客户端同口径放在 `spawn_blocking`（不占 async 执行器）。
pub async fn dated_notes(
    State(state): State<ServerState>,
    user: AuthUser,
    AxumPath(space_id): AxumPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = member_root(&state, &space_id, &user)?;
    let exclude = crate::meta::space_exclusions(state.data_dir(), &space_id);
    let root_path = root.0.clone();
    let notes = tokio::task::spawn_blocking(move || {
        let mut notes: Vec<DatedNote> = Vec::new();
        let _ = walk_md_in(&root_path, "", &exclude, &mut |rel, path| {
            if notes.len() >= DATED_NOTE_CAP {
                return Ok(());
            }
            let Some(head) = read_head(path, MD_HEAD_CAP) else {
                return Ok(());
            };
            let (date, due) = frontmatter_block(&head)
                .map(frontmatter_date_values)
                .unwrap_or((None, None));
            let date = date.as_deref().and_then(extract_ymd);
            let due = due.as_deref().and_then(extract_ymd);
            if date.is_none() && due.is_none() {
                return Ok(());
            }
            let title = Path::new(rel)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel.to_string());
            notes.push(DatedNote {
                file: rel.to_string(),
                title,
                date,
                due,
            });
            Ok(())
        });
        notes.sort_by(|a, b| a.file.cmp(&b.file));
        notes
    })
    .await
    .map_err(|e| crate::ApiError(axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("扫描线程失败：{e}")))?;
    Ok(Json(json!(notes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ymd_from_various_values() {
        assert_eq!(extract_ymd("2024-01-15"), Some("2024-01-15".to_string()));
        assert_eq!(extract_ymd("2024-01-15T10:00:00"), Some("2024-01-15".to_string()));
        assert_eq!(extract_ymd("无日期"), None);
        assert_eq!(extract_ymd("2024-1-5"), None);
    }

    #[test]
    fn reads_frontmatter_block_dates() {
        let head = "---\ntitle: x\ndate: 2024-03-01\ndue: \"2024-03-05\"\n---\n正文";
        let fm = frontmatter_block(head).unwrap();
        let (d, u) = frontmatter_date_values(fm);
        assert_eq!(d.as_deref().and_then(extract_ymd), Some("2024-03-01".to_string()));
        assert_eq!(u.as_deref().and_then(extract_ymd), Some("2024-03-05".to_string()));
    }
}
