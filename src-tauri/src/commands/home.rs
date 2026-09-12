//! 主页面板数据命令（日历/仓库历史）：带日期笔记扫描 + 全仓库历史版本聚合。
//!
//! 两类都是只读聚合，尽力而为：缺失/损坏文件静默跳过，不阻塞面板显示。
//! - `list_dated_notes`：扫描仓库 `.md` frontmatter 的 `date`/`due` 字段（自动进日历）。
//! - `list_repo_history`：聚合 `.atelyx/history/` 全部版本（版本流 + 按日计数），
//!   剔除全文 content 以控制载荷；日期按本机时区归日（活动日历与用户日历一致）。

use chrono::{DateTime, Local, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use tauri::State;

use crate::vault::{percent_decode, walk_md_in, VaultState};

/// 带日期笔记（frontmatter `date`/`due`，自动进日历）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DatedNote {
    pub file: String,
    pub title: String,
    pub date: Option<String>,
    pub due: Option<String>,
}

/// 历史版本元数据（聚合展示，不含全文 content 以控制载荷）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RepoHistoryEntry {
    pub file: String,
    pub kind: String,
    pub ts: i64,
    pub author_id: String,
    pub author_name: String,
    pub author_device: String,
    pub action: String,
    pub summary: Option<String>,
    pub note: Option<String>,
}

/// 按日历史版本计数（活动日历，全量不计上限）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DailyCount {
    /// 本地日期 "YYYY-MM-DD"。
    pub date: String,
    pub count: i64,
}

/// list_repo_history 返回：版本流（ts 倒序、上限）+ 全量按日计数（活动日历，不受上限截断）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoHistoryResult {
    pub entries: Vec<RepoHistoryEntry>,
    pub daily_counts: Vec<DailyCount>,
}

/// 版本流上限（大仓库防 IPC 载荷爆炸；活动日历走 daily_counts 不受此限）。
const HISTORY_FEED_CAP: usize = 300;
/// 带日期笔记扫描上限（防御性；超过即截断，日历仍有手动日程兜底）。
const DATED_NOTE_CAP: usize = 2000;
/// 单个 .md 读取字节上限（frontmatter 解析只需文件头）。
const MD_HEAD_CAP: usize = 8192;

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

/// 扫描仓库 `.md` frontmatter 的 date/due 字段，返回带日期笔记（尽力而为：读失败/无日期跳过）。
/// 全仓库扫描是纯阻塞 IO：放 `spawn_blocking`，不占 async 执行器（否则与同运行时的其他命令互相排队）。
#[tauri::command]
pub async fn list_dated_notes(state: State<'_, VaultState>) -> Result<Vec<DatedNote>, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut notes: Vec<DatedNote> = Vec::new();
        let _ = walk_md_in(&root, "", &exclude, &mut |rel, path| {
            if notes.len() >= DATED_NOTE_CAP {
                return Ok(());
            }
            let head = match read_head(path, MD_HEAD_CAP) {
                Some(h) => h,
                None => return Ok(()),
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
        Ok(notes)
    })
    .await
    .map_err(|e| format!("扫描线程失败：{e}"))?
}

// ===== 历史版本聚合 =====

/// 历史侧文件解析（前端 `services/history` 同 schema；seq/content 等未知字段 serde 忽略）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryFile {
    versions: Vec<HistoryVersion>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryVersion {
    ts: i64,
    author: HistoryAuthor,
    action: String,
    summary: Option<String>,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryAuthor {
    id: String,
    name: String,
    device: String,
}

/// 递归收集 `.atelyx/history/` 下全部版本（kind 由子目录名判定；根目录 = 旧 note 路径）。
/// 注意：子目录白名单须与前端 `services/history/index.ts` 的 `historyPathFor` 保持一致
/// （新增 kind 目录需两端同步，否则静默漏扫）。
fn collect_history_dir(dir: &Path, kind: &str, out: &mut Vec<RepoHistoryEntry>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() && (name == "canvas" || name == "table") {
            collect_history_dir(&path, &name, out);
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        let file = percent_decode(&name[..name.len() - 5]);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<HistoryFile>(&raw) else {
            continue;
        };
        for v in parsed.versions {
            out.push(RepoHistoryEntry {
                file: file.clone(),
                kind: kind.to_string(),
                ts: v.ts,
                author_id: v.author.id,
                author_name: v.author.name,
                author_device: v.author.device,
                action: v.action,
                summary: v.summary,
                note: v.note,
            });
        }
    }
}

/// 毫秒时间戳 → 本机时区日期 "YYYY-MM-DD"。
fn ts_to_ymd(ts_ms: i64) -> Option<String> {
    let secs = ts_ms / 1000;
    let dt = DateTime::<Utc>::from_timestamp(secs, 0)?;
    let local = dt.with_timezone(&Local);
    Some(local.format("%Y-%m-%d").to_string())
}

/// 聚合 `.atelyx/history/` 全部版本：版本流（ts 倒序、上限）+ 全量按日计数。
/// 尽力而为：缺失/损坏侧文件跳过，不阻塞面板。
/// 目录聚合是纯阻塞 IO：放 `spawn_blocking`，不占 async 执行器。
#[tauri::command]
pub async fn list_repo_history(state: State<'_, VaultState>) -> Result<RepoHistoryResult, String> {
    let root = state.root()?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut entries: Vec<RepoHistoryEntry> = Vec::new();
        collect_history_dir(&root.join(".atelyx/history"), "note", &mut entries);
        // 全量按日计数（活动日历，不受版本流上限截断）
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for e in &entries {
            if let Some(date) = ts_to_ymd(e.ts) {
                *counts.entry(date).or_insert(0) += 1;
            }
        }
        let daily_counts = counts
            .into_iter()
            .map(|(date, count)| DailyCount { date, count })
            .collect();
        entries.sort_by(|a, b| b.ts.cmp(&a.ts));
        entries.truncate(HISTORY_FEED_CAP);
        Ok(RepoHistoryResult {
            entries,
            daily_counts,
        })
    })
    .await
    .map_err(|e| format!("聚合线程失败：{e}"))?
}
