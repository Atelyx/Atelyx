//! 文件历史侧车（笔记/画布/表格）：每文件一个隐藏 JSON 侧文件（全文快照 + 行级 diff 摘要），独立于磁盘真相的审计/回滚轴。
//! 侧文件格式为 `{ versions: [...] }`，path = `.atelyx/history/[<kind>/]<最小编码>.json`（canvas/table 有 kind 子目录）；
//! 记录尽力而为——读缺失/损坏返回空、写失败由调用方决定降级。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 历史作用的文件类型（决定侧文件路径）；note 顶层，canvas/table 按 kind 分子目录。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryKind {
    Note,
    Canvas,
    Table,
}

impl HistoryKind {
    /// kind 子目录名（空 = 顶层；无尾斜杠）。
    fn dir(self) -> &'static str {
        match self {
            HistoryKind::Note => "",
            HistoryKind::Canvas => "canvas",
            HistoryKind::Table => "table",
        }
    }
}

/// 版本作者身份。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAuthor {
    pub id: String,
    pub name: String,
    pub device: String,
}

/// 版本行为（edit = 常规编辑；restore = 回滚落盘）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryAction {
    Edit,
    Restore,
}

/// 一个历史版本（全文快照；summary = 相对上一版的展示摘要）。
/// `coAuthors` = 协作署名（由协作侧写入的字段，读侧解析保留、coalesce 滑动更新原样带出）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryVersion {
    pub seq: u64,
    pub ts: i64,
    pub author: HistoryAuthor,
    pub action: HistoryAction,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub co_authors: Option<Vec<HistoryAuthor>>,
}

#[derive(Serialize, Deserialize, Default)]
struct HistoryFile {
    #[serde(default)]
    versions: Vec<HistoryVersion>,
}

/// 侧文件字节预算（超限从最旧剪起，仅剩 1 版也超则保留兜底）。
pub const HISTORY_BYTE_BUDGET: usize = 5_000_000;

/// 侧文件相对路径（note 顶层，canvas/table 在 kind 子目录；文件名最小编码与 vault 的 percent_encode 同字符集）。
pub fn history_path_for(kind: HistoryKind, file: &str) -> String {
    let encoded = crate::vault::percent_encode(file);
    let dir = kind.dir();
    if dir.is_empty() {
        format!(".atelyx/history/{encoded}.json")
    } else {
        format!(".atelyx/history/{dir}/{encoded}.json")
    }
}

/// 存量侧文件名（全量百分号编码的旧路径；新名缺失时读侧回退）。
fn legacy_history_path_for(kind: HistoryKind, file: &str) -> String {
    let encoded = crate::vault::percent_encode_legacy(file);
    let dir = kind.dir();
    if dir.is_empty() {
        format!(".atelyx/history/{encoded}.json")
    } else {
        format!(".atelyx/history/{dir}/{encoded}.json")
    }
}

/// 读取侧文件版本列表：新编码名优先，缺失回退旧编码存量名；缺失/损坏 → 空列表。
pub fn load_history(root: &Path, kind: HistoryKind, file: &str) -> Vec<HistoryVersion> {
    for path in [history_path_for(kind, file), legacy_history_path_for(kind, file)] {
        let Ok(raw) = std::fs::read_to_string(root.join(path)) else {
            continue;
        };
        if let Ok(parsed) = serde_json::from_str::<HistoryFile>(&raw) {
            return parsed.versions;
        }
    }
    Vec::new()
}

// ===== 仓库级聚合（活动日历/历史面板用）=====

/// 聚合条目的版本元数据：刻意不含 content——serde 默认忽略未知字段，全文快照解析期
/// 被丢弃不进内存（仓库聚合与活动日历只需元数据）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionMeta {
    pub seq: u64,
    pub ts: i64,
    pub action: HistoryAction,
    #[serde(default)]
    pub summary: Option<String>,
    pub author: HistoryAuthor,
}

#[derive(Deserialize)]
struct HistoryMetaFile {
    #[serde(default)]
    versions: Vec<VersionMeta>,
}

/// 仓库历史聚合条目：一个侧文件的归属 kind、解码回的文件相对路径与全部版本元数据。
#[derive(Clone, Debug, Serialize)]
pub struct RepoHistoryFile {
    pub kind: HistoryKind,
    /// percent_decode 还原的文件相对路径（与 percent_encode/history_path_for 对应）。
    pub file: String,
    pub versions: Vec<VersionMeta>,
}

/// 扫描 `.atelyx/history/` 全部侧文件的版本元数据（不读全文快照）：顶层 = note，
/// `canvas/`、`table/` 子目录 = 同名 kind（与 history_path_for 同源，其余子目录忽略）。
/// 损坏/不可读侧文件跳过（聚合尽力而为，与 load_history 同口径）；目录缺失 = 空。
pub fn collect_all(root: &Path) -> Result<Vec<RepoHistoryFile>, String> {
    let history_dir = root.join(".atelyx/history");
    let mut out = Vec::new();
    scan_kind_dir(&history_dir, HistoryKind::Note, &mut out)?;
    scan_kind_dir(&history_dir.join("canvas"), HistoryKind::Canvas, &mut out)?;
    scan_kind_dir(&history_dir.join("table"), HistoryKind::Table, &mut out)?;
    Ok(out)
}

fn scan_kind_dir(dir: &Path, kind: HistoryKind, out: &mut Vec<RepoHistoryFile>) -> Result<(), String> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    for entry in rd.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if !name.ends_with(".json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<HistoryMetaFile>(&raw) else {
            continue;
        };
        out.push(RepoHistoryFile {
            kind,
            file: crate::vault::percent_decode(&name[..name.len() - 5]),
            versions: file.versions,
        });
    }
    Ok(())
}

/// 当前本地时区相对 UTC 的偏移（毫秒，东正西负）。对全部时间戳统一用当前偏移折算
/// 本地日界（与展示层对历史时间的换算口径一致，不逐时间戳回算历史时区）；取不到按 UTC。
#[cfg(target_os = "windows")]
fn local_utc_offset_ms() -> i64 {
    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
    }
    let mut info = TimeZoneInformation {
        bias: 0,
        standard_name: [0; 32],
        standard_date: SystemTime { year: 0, month: 0, day_of_week: 0, day: 0, hour: 0, minute: 0, second: 0, millis: 0 },
        standard_bias: 0,
        daylight_name: [0; 32],
        daylight_date: SystemTime { year: 0, month: 0, day_of_week: 0, day: 0, hour: 0, minute: 0, second: 0, millis: 0 },
        daylight_bias: 0,
    };
    // 返回值：0 未知 / 1 标准时 / 2 夏令时；UTC = 本地 + bias + 当前段 bias（分钟），本地偏移取负
    let id = unsafe { GetTimeZoneInformation(&mut info) };
    let extra = if id == 2 { info.daylight_bias } else { info.standard_bias };
    -((info.bias + extra) as i64) * 60_000
}

#[cfg(unix)]
fn local_utc_offset_ms() -> i64 {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff * 1000
    }
}

#[cfg(not(any(target_os = "windows", unix)))]
fn local_utc_offset_ms() -> i64 {
    0
}

/// 时间戳（毫秒）按本地日界折算进计数表（key = 自纪元天数）。
fn bump_day(counts: &mut HashMap<i64, u32>, ts_ms: i64, offset_ms: i64) {
    *counts.entry(day_key_with(ts_ms, offset_ms)).or_insert(0) += 1;
}

/// 毫秒时间戳按 `offset_ms` 时区偏移折算自纪元天数（`offset_ms` 可注入便于测试）。
/// 活动密度计数与展示层按日过滤共用此口径，避免两处实现漂移。
pub fn day_key_with(ts_ms: i64, offset_ms: i64) -> i64 {
    (ts_ms + offset_ms).div_euclid(86_400_000)
}

/// `day_key_with` 取当前系统时区偏移。
pub fn day_key(ts_ms: i64) -> i64 {
    day_key_with(ts_ms, local_utc_offset_ms())
}

/// 毫秒时间戳 → 本地时刻 (时, 分)（当日版本行的展示粒度）。
pub fn local_time_hm(ts_ms: i64) -> (u32, u32) {
    let secs = (ts_ms + local_utc_offset_ms()).div_euclid(1000).rem_euclid(86_400);
    ((secs / 3600) as u32, ((secs % 3600) / 60) as u32)
}

/// 当前本地时区相对 UTC 的偏移（毫秒，东正西负）；展示层历史时间换算与 `local_time_hm` 同口径。
pub fn local_offset_ms() -> i64 {
    local_utc_offset_ms()
}

/// 活动密度：`.atelyx/history` 全部版本按日计数 + 全库可见文件 mtime 按日计数
/// （read_dir_filtered 同过滤：隐藏与排除目录不计）。key = 本地日界的自纪元天数。
/// 本地仓库用；协作空间由调用方走服务端聚合。尽力而为：损坏侧文件与读失败文件跳过。
pub fn activity_by_day(root: &Path, exclude: &[String]) -> HashMap<i64, u32> {
    let offset = local_utc_offset_ms();
    let mut counts: HashMap<i64, u32> = HashMap::new();
    if let Ok(files) = collect_all(root) {
        for f in files {
            for v in f.versions {
                bump_day(&mut counts, v.ts, offset);
            }
        }
    }
    count_mtime_by_day(root, exclude, offset, &mut counts);
    counts
}

fn count_mtime_by_day(root: &Path, exclude: &[String], offset_ms: i64, counts: &mut HashMap<i64, u32>) {
    fn walk(rel: &str, root: &Path, exclude: &[String], offset_ms: i64, counts: &mut HashMap<i64, u32>) {
        let Ok(children) = crate::vault::read_dir_filtered(root, rel, exclude) else {
            return;
        };
        for (child_rel, is_dir) in children {
            if is_dir {
                walk(&child_rel, root, exclude, offset_ms, counts);
            } else if let Ok(modified) = std::fs::metadata(root.join(&child_rel)).and_then(|m| m.modified()) {
                let ms = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                // mtime 不可得（0）不计（与文件树 updatedAt 的展示口径一致）
                if ms > 0 {
                    bump_day(counts, ms, offset_ms);
                }
            }
        }
    }
    walk("", root, exclude, offset_ms, counts);
}

/// 追加版本的入参。
pub struct RecordOpts {
    pub content: String,
    pub action: HistoryAction,
    pub author: HistoryAuthor,
    /// 连续编辑合并窗口（毫秒）：上一版为同作者 edit 且距今 < 该值 → 滑动更新不新增 seq。
    pub coalesce_edit_ms: Option<i64>,
    /// 版本数上限（0 = 全留）。
    pub max_versions: usize,
    /// 补充说明。
    pub note: Option<String>,
}

/// 追加一个历史版本（本地路径全量读改写，无锁——调用方需避免并发写同一侧文件；
/// 内容与上一版相同则跳过防重复存档点）。
/// 写失败向上传播，调用方按「历史尽力而为」降级（不得阻塞正文保存）。
pub fn record_version(root: &Path, kind: HistoryKind, file: &str, opts: &RecordOpts) -> Result<(), String> {
    let mut versions = load_history(root, kind, file);
    if let Some(last) = versions.last() {
        // 内容 no-op：不产生新版本（回滚后同内容的常规保存同样跳过）
        if last.content == opts.content {
            return Ok(());
        }
    }
    let now = now_ms();
    let coalesce = match (&versions.last(), opts.coalesce_edit_ms) {
        (Some(last), Some(window)) if opts.action == HistoryAction::Edit
            && last.action == HistoryAction::Edit
            && last.author.id == opts.author.id
            && now - last.ts < window =>
        {
            Some(last.seq)
        }
        _ => None,
    };
    if coalesce.is_some() {
        let prev = versions
            .iter()
            .rev()
            .nth(1)
            .map(|v| v.content.clone())
            .unwrap_or_default();
        let last = versions.last_mut().unwrap();
        last.content = opts.content.clone();
        last.ts = now;
        last.author = opts.author.clone();
        last.summary = Some(diff_summary(&prev, &opts.content));
        if opts.note.is_some() {
            last.note = opts.note.clone();
        }
        return write_history(root, kind, file, &versions);
    }
    let prev_content = versions.last().map(|v| v.content.clone()).unwrap_or_default();
    let seq = versions.last().map(|v| v.seq + 1).unwrap_or(1);
    let version = HistoryVersion {
        seq,
        ts: now,
        author: opts.author.clone(),
        action: opts.action,
        content: opts.content.clone(),
        summary: Some(diff_summary(&prev_content, &opts.content)),
        note: opts.note.clone(),
        co_authors: None,
    };
    versions.push(version);
    let max = opts.max_versions;
    if max > 0 && versions.len() > max {
        versions = versions.split_off(versions.len() - max);
    }
    versions = prune_bytes(versions, HISTORY_BYTE_BUDGET);
    write_history(root, kind, file, &versions)
}

fn write_history(root: &Path, kind: HistoryKind, file: &str, versions: &[HistoryVersion]) -> Result<(), String> {
    let json = serde_json::to_string(&HistoryFile { versions: versions.to_vec() }).map_err(|e| e.to_string())?;
    crate::vault::atomic_write(&root.join(history_path_for(kind, file)), &json)
}

// ===== 版本回写（回滚链末端）=====

/// 快照可按 kind 解析（table/canvas 另校验 schema，防把当前版本读不回来的文件写进仓库）。
pub fn validate_snapshot(kind: HistoryKind, content: &str) -> Result<(), String> {
    match kind {
        HistoryKind::Note => Ok(()),
        HistoryKind::Table => {
            let table: crate::vault::TableFile =
                serde_json::from_str(content).map_err(|e| format!("表格快照无法解析：{e}"))?;
            if table.schema != crate::vault::TABLE_SCHEMA {
                return Err(format!("表格快照 schema 不匹配：{}", table.schema));
            }
            Ok(())
        }
        HistoryKind::Canvas => {
            let canvas: crate::vault::CanvasFile =
                serde_json::from_str(content).map_err(|e| format!("画布快照无法解析：{e}"))?;
            if canvas.schema != crate::vault::CANVAS_SCHEMA {
                return Err(format!("画布快照 schema 不匹配：{}", canvas.schema));
            }
            Ok(())
        }
    }
}

/// 版本快照写回磁盘：先按 kind 校验快照（[`validate_snapshot`]），再按原文原子写回
/// （不重排格式，磁盘字节 = 快照字节）。壳侧经写盘队列批量作业调用（与其它写盘串行）；
/// 校验失败返回 Err 且不落盘。
pub fn restore_snapshot(root: &Path, kind: HistoryKind, file: &str, content: &str) -> Result<(), String> {
    validate_snapshot(kind, content)?;
    let path = crate::vault::safe_join(root, file, false)?;
    crate::vault::atomic_write(&path, content)
}

/// 字节预算剪枝：超限从最旧丢，仅剩 1 版也超则保留兜底。
fn prune_bytes(versions: Vec<HistoryVersion>, budget: usize) -> Vec<HistoryVersion> {
    let mut next = versions;
    let size = |v: &[HistoryVersion]| serde_json::to_string(&HistoryFile { versions: v.to_vec() }).map(|s| s.len()).unwrap_or(usize::MAX);
    while next.len() > 1 && size(&next) > budget {
        next.remove(0);
    }
    next
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 行级 diff 摘要（+N 行 −M 行；纯增删字符时回退字数估算；未变 = 「未改动」）。
pub fn diff_summary(prev: &str, next: &str) -> String {
    if prev == next {
        return "未改动".into();
    }
    let a: Vec<&str> = prev.split('\n').collect();
    let b: Vec<&str> = next.split('\n').collect();
    let count_added = |list: &[&str], other: &[&str]| -> usize {
        let common: std::collections::HashSet<&str> = other.iter().copied().collect();
        list.iter().filter(|l| !l.is_empty() && !common.contains(**l)).count()
    };
    let added = count_added(&b, &a);
    let removed = count_added(&a, &b);
    if added == 0 && removed == 0 {
        let len_diff = next.chars().count() as i64 - prev.chars().count() as i64;
        return if len_diff >= 0 { format!("+{len_diff} 字") } else { format!("{len_diff} 字") };
    }
    let mut out = String::new();
    if added > 0 {
        out.push_str(&format!("+{added} 行 "));
    }
    if removed > 0 {
        out.push_str(&format!("−{removed} 行"));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author() -> HistoryAuthor {
        HistoryAuthor { id: "t".into(), name: "T".into(), device: "T".into() }
    }

    fn opts(content: &str) -> RecordOpts {
        RecordOpts {
            content: content.into(),
            action: HistoryAction::Edit,
            author: author(),
            coalesce_edit_ms: Some(60_000),
            max_versions: 0,
            note: None,
        }
    }

    #[test]
    fn append_coalesce_and_skip() {
        let tmp = crate::vault::test_support::TempDir::new("hist-basic");
        record_version(&tmp, HistoryKind::Note, "a.md", &opts("v1")).unwrap();
        // 内容 no-op：跳过
        record_version(&tmp, HistoryKind::Note, "a.md", &opts("v1")).unwrap();
        assert_eq!(load_history(&tmp, HistoryKind::Note, "a.md").len(), 1);
        // 窗口内连续编辑：滑动更新不新增 seq
        record_version(&tmp, HistoryKind::Note, "a.md", &opts("v2")).unwrap();
        let versions = load_history(&tmp, HistoryKind::Note, "a.md");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].content, "v2");
        assert_eq!(versions[0].seq, 1);
    }

    #[test]
    fn restore_action_breaks_coalesce() {
        let tmp = crate::vault::test_support::TempDir::new("hist-restore");
        record_version(&tmp, HistoryKind::Note, "a.md", &opts("v1")).unwrap();
        record_version(
            &tmp,
            HistoryKind::Note,
            "a.md",
            &RecordOpts {
                content: "v2".into(),
                action: HistoryAction::Restore,
                author: author(),
                coalesce_edit_ms: Some(60_000),
                max_versions: 0,
                note: None,
            },
        )
        .unwrap();
        let versions = load_history(&tmp, HistoryKind::Note, "a.md");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].action, HistoryAction::Restore);
    }

    #[test]
    fn max_versions_prunes_oldest() {
        let tmp = crate::vault::test_support::TempDir::new("hist-prune");
        for i in 0..5 {
            let mut o = opts(&format!("v{i}"));
            o.coalesce_edit_ms = None;
            record_version(&tmp, HistoryKind::Note, "a.md", &o).unwrap();
        }
        let mut o = opts("v5");
        o.max_versions = 3;
        o.coalesce_edit_ms = None;
        record_version(&tmp, HistoryKind::Note, "a.md", &o).unwrap();
        let versions = load_history(&tmp, HistoryKind::Note, "a.md");
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].content, "v3");
    }

    #[test]
    fn read_falls_back_to_legacy_name() {
        let tmp = crate::vault::test_support::TempDir::new("hist-legacy");
        // 旧编码名下的存量侧文件（encodeURIComponent 全量转义形态）
        let dir = tmp.join(".atelyx/history");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("a%20b.md.json");
        std::fs::write(&legacy, r#"{"versions":[{"seq":1,"ts":1,"author":{"id":"x","name":"x","device":"x"},"action":"edit","content":"old"}]}"#).unwrap();
        let versions = load_history(&tmp, HistoryKind::Note, "a b.md");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].content, "old");
    }

    #[test]
    fn restore_snapshot_canvas_rollback_writes_exact_bytes() {
        let tmp = crate::vault::test_support::TempDir::new("hist-restore-snap");
        // 磁盘当前内容（v2）+ 历史侧文件里的旧快照（v1）
        let v1 = r#"{"schema":"atelyx-canvas/v1","id":"c1","title":"画布","nodes":[{"id":"n1","type":"text","x":1.0,"y":2.0,"data":{"text":"旧"}}],"edges":[]}"#;
        let v2 = r#"{"schema":"atelyx-canvas/v1","id":"c1","title":"画布","nodes":[{"id":"n1","type":"text","x":9.0,"y":9.0,"data":{"text":"新"}}],"edges":[]}"#;
        std::fs::write(tmp.join("画布.atlx"), v2).unwrap();
        record_version(&tmp, HistoryKind::Canvas, "画布.atlx", &opts(v1)).unwrap();
        assert_eq!(load_history(&tmp, HistoryKind::Canvas, "画布.atlx").len(), 1);
        // 回滚到 v1：磁盘字节 = 快照字节（不重排格式）
        restore_snapshot(&tmp, HistoryKind::Canvas, "画布.atlx", v1).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.join("画布.atlx")).unwrap(), v1);
        // 非法快照：拒绝且磁盘不动
        assert!(restore_snapshot(&tmp, HistoryKind::Canvas, "画布.atlx", "{oops").is_err());
        assert_eq!(std::fs::read_to_string(tmp.join("画布.atlx")).unwrap(), v1);
        // schema 不匹配：拒绝且磁盘不动
        let bad_schema = v1.replace("atelyx-canvas/v1", "atelyx-canvas/v9");
        assert!(restore_snapshot(&tmp, HistoryKind::Canvas, "画布.atlx", &bad_schema).is_err());
        assert_eq!(std::fs::read_to_string(tmp.join("画布.atlx")).unwrap(), v1);
    }

    #[test]
    fn restore_snapshot_validates_table_and_note_plain_write() {
        let tmp = crate::vault::test_support::TempDir::new("hist-restore-snap2");
        // note 无结构校验：原样写
        std::fs::write(tmp.join("a.md"), "新").unwrap();
        restore_snapshot(&tmp, HistoryKind::Note, "a.md", "旧").unwrap();
        assert_eq!(std::fs::read_to_string(tmp.join("a.md")).unwrap(), "旧");
        // table 快照解析失败：拒绝
        assert!(restore_snapshot(&tmp, HistoryKind::Table, "t.atb", "not json").is_err());
        // table schema 不匹配：拒绝
        let bad = r#"{"schema":"atelyx-table/v9","id":"t","title":"x"}"#;
        assert!(restore_snapshot(&tmp, HistoryKind::Table, "t.atb", bad).is_err());
        // 合法 table 快照：写盘成功
        let good = r#"{"schema":"atelyx-table/v1","id":"t","title":"表格","fields":[],"rows":[]}"#;
        restore_snapshot(&tmp, HistoryKind::Table, "t.atb", good).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.join("t.atb")).unwrap(), good);
        // 越界路径：safe_join 拒绝
        assert!(restore_snapshot(&tmp, HistoryKind::Note, "../escape.md", "x").is_err());
    }

    #[test]
    fn diff_summary_lines_and_chars() {
        assert_eq!(diff_summary("a\nb", "a\nc"), "+1 行 −1 行");
        // 单行替换按整行增删计
        assert_eq!(diff_summary("abc", "abd"), "+1 行 −1 行");
        // 行集合不变（重复行同集合）：回退字数估算
        assert_eq!(diff_summary("a\na", "a\na\na"), "+2 字");
        assert_eq!(diff_summary("same", "same"), "未改动");
    }

    #[test]
    fn history_path_layout_and_encoding() {
        assert_eq!(history_path_for(HistoryKind::Note, "a.md"), ".atelyx/history/a.md.json");
        assert_eq!(history_path_for(HistoryKind::Canvas, "画布.atlx"), ".atelyx/history/canvas/画布.atlx.json");
        assert_eq!(history_path_for(HistoryKind::Table, "t.atb"), ".atelyx/history/table/t.atb.json");
        // 非法字符最小编码，percent_decode 还原（collect_all 的 file 字段同源）
        assert_eq!(
            history_path_for(HistoryKind::Canvas, "a:b/c.atlx"),
            ".atelyx/history/canvas/a%3Ab%2Fc.atlx.json"
        );
        assert_eq!(crate::vault::percent_decode("a%3Ab%2Fc.atlx"), "a:b/c.atlx");
    }

    #[test]
    fn canvas_history_roundtrip_and_collect() {
        let tmp = crate::vault::test_support::TempDir::new("hist-canvas");
        record_version(&tmp, HistoryKind::Canvas, "画布.atlx", &opts("v1")).unwrap();
        // 路径规则写入后按同规则读回
        assert_eq!(load_history(&tmp, HistoryKind::Canvas, "画布.atlx").len(), 1);
        let files = collect_all(&tmp).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, HistoryKind::Canvas);
        assert_eq!(files[0].file, "画布.atlx");
        assert_eq!(files[0].versions.len(), 1);
        assert_eq!(files[0].versions[0].seq, 1);
        assert_eq!(files[0].versions[0].action, HistoryAction::Edit);
        assert_eq!(files[0].versions[0].author, author());
    }

    #[test]
    fn collect_all_groups_kinds_and_ignores_junk() {
        let tmp = crate::vault::test_support::TempDir::new("hist-collect");
        let root = &*tmp;
        let hist = root.join(".atelyx/history");
        std::fs::create_dir_all(hist.join("canvas")).unwrap();
        std::fs::create_dir_all(hist.join("table")).unwrap();
        std::fs::create_dir_all(hist.join("other")).unwrap();
        // 大 content：聚合只取元数据（VersionMeta 无 content 字段），大文本不 panic 不驻留
        let big = "x".repeat(1_000_000);
        let v = |seq: u64, ts: i64| {
            format!(
                r#"{{"seq":{seq},"ts":{ts},"author":{{"id":"t","name":"T","device":"T"}},"action":"edit","summary":"s","content":"{big}"}}"#
            )
        };
        std::fs::write(hist.join("a b.md.json"), format!(r#"{{"versions":[{},{}]}}"#, v(1, 100), v(2, 200))).unwrap();
        std::fs::write(hist.join("canvas/画%3A布.atlx.json"), format!(r#"{{"versions":[{}]}}"#, v(1, 300))).unwrap();
        std::fs::write(hist.join("table/t.atb.json"), r#"{"versions":[]}"#).unwrap();
        // 非白名单子目录 / 非 json / 损坏 json：跳过
        std::fs::write(hist.join("other/x.json"), r#"{"versions":[]}"#).unwrap();
        std::fs::write(hist.join("note.txt"), "not json").unwrap();
        std::fs::write(hist.join("broken.json"), "{oops").unwrap();

        let files = collect_all(root).unwrap();
        assert_eq!(files.len(), 3);
        let note = files.iter().find(|f| f.file == "a b.md").unwrap();
        assert_eq!(note.kind, HistoryKind::Note);
        assert_eq!(note.versions.len(), 2);
        assert_eq!(note.versions[1].seq, 2);
        assert_eq!(note.versions[1].ts, 200);
        assert_eq!(note.versions[1].summary.as_deref(), Some("s"));
        let canvas = files.iter().find(|f| f.file == "画:布.atlx").unwrap();
        assert_eq!(canvas.kind, HistoryKind::Canvas);
        assert_eq!(canvas.versions[0].ts, 300);
        let table = files.iter().find(|f| f.file == "t.atb").unwrap();
        assert_eq!(table.kind, HistoryKind::Table);
        assert!(table.versions.is_empty());
    }

    #[test]
    fn activity_by_day_sums_history_and_mtime() {
        let tmp = crate::vault::test_support::TempDir::new("hist-activity");
        let root = &*tmp;
        // 可见文件 mtime = 创建时刻（今天）：2 个；先建文件再取 now，两侧同日
        std::fs::write(root.join("a.md"), "x").unwrap();
        std::fs::write(root.join("b.md"), "y").unwrap();
        let now = now_ms();
        let today = (now + local_utc_offset_ms()).div_euclid(86_400_000);
        // 偏移取常量口径：±24h 恒跨一天
        let yesterday = today - 1;
        // 历史：今天 1 版 + 昨天 2 版（手工写侧文件固定 ts；canvas 子目录顺带覆盖扫描）
        let dir = root.join(".atelyx/history/canvas");
        std::fs::create_dir_all(&dir).unwrap();
        let v = |ts: i64| {
            format!(r#"{{"seq":1,"ts":{ts},"author":{{"id":"t","name":"T","device":"T"}},"action":"edit","content":"c"}}"#)
        };
        std::fs::write(
            dir.join("c.atlx.json"),
            format!(r#"{{"versions":[{},{},{}]}}"#, v(now), v(now - 86_400_000), v(now - 86_400_000)),
        )
        .unwrap();
        // `.atelyx/` 侧文件自身走隐藏目录过滤，不进 mtime 口径
        let counts = activity_by_day(root, &[]);
        assert_eq!(counts.len(), 2);
        assert_eq!(counts.get(&today), Some(&3));
        assert_eq!(counts.get(&yesterday), Some(&2));
    }

    #[test]
    fn activity_by_day_respects_exclude() {
        let tmp = crate::vault::test_support::TempDir::new("hist-activity-excl");
        let root = &*tmp;
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("secret/c.md"), "x").unwrap();
        let today = (now_ms() + local_utc_offset_ms()).div_euclid(86_400_000);
        assert_eq!(activity_by_day(root, &["secret".to_string()]).get(&today), None);
        assert_eq!(activity_by_day(root, &[]).get(&today), Some(&1));
    }

    #[test]
    fn day_key_splits_at_local_midnight() {
        // 偏移 0 = UTC 日界；东八区日界在 UTC 16:00；西五区日界在 UTC 05:00
        assert_eq!(day_key_with(0, 0), 0);
        assert_eq!(day_key_with(86_399_999, 0), 0);
        assert_eq!(day_key_with(86_400_000, 0), 1);
        assert_eq!(day_key_with(15 * 3_600_000 - 1, 8 * 3_600_000), 0);
        assert_eq!(day_key_with(16 * 3_600_000, 8 * 3_600_000), 1);
        // 西五区日界在 UTC 05:00（05:00 前属前一本地日）
        assert_eq!(day_key_with(4 * 3_600_000 + 3_599_999, -5 * 3_600_000), -1);
        assert_eq!(day_key_with(5 * 3_600_000, -5 * 3_600_000), 0);
        // 负时刻（1970 前）同样按偏移归日
        assert_eq!(day_key_with(-1, 0), -1);
        assert_eq!(day_key_with(-8 * 3_600_000 - 1, 8 * 3_600_000), -1);
    }
}
