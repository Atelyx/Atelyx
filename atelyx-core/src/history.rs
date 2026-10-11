//! 笔记历史侧车：每文件一个隐藏 JSON 侧文件（全文快照 + 行级 diff 摘要），独立于磁盘真相的审计/回滚轴。
//!
//! 侧文件格式为 `{ versions: [...] }`，path = `.atelyx/history/<最小编码>.json`；
//! 记录尽力而为——读缺失/损坏返回空、写失败由调用方决定降级。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 历史作用的文件类型（决定侧文件路径）；当前仅笔记（note 顶层），画布/表格按 kind 分目录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryKind {
    Note,
}

impl HistoryKind {
    fn dir(self) -> &'static str {
        match self {
            HistoryKind::Note => "",
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

/// 侧文件相对路径（note 顶层；文件名最小编码与 vault 的 percent_encode 同字符集）。
pub fn history_path_for(kind: HistoryKind, file: &str) -> String {
    let encoded = crate::vault::percent_encode(file);
    if kind.dir().is_empty() {
        format!(".atelyx/history/{encoded}.json")
    } else {
        format!(".atelyx/history/{}/{}.json", kind.dir(), encoded)
    }
}

/// 存量侧文件名（全量百分号编码的旧路径；新名缺失时读侧回退）。
fn legacy_history_path_for(kind: HistoryKind, file: &str) -> String {
    let encoded = crate::vault::percent_encode_legacy(file);
    if kind.dir().is_empty() {
        format!(".atelyx/history/{encoded}.json")
    } else {
        format!(".atelyx/history/{}/{}.json", kind.dir(), encoded)
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
    fn diff_summary_lines_and_chars() {
        assert_eq!(diff_summary("a\nb", "a\nc"), "+1 行 −1 行");
        // 单行替换按整行增删计
        assert_eq!(diff_summary("abc", "abd"), "+1 行 −1 行");
        // 行集合不变（重复行同集合）：回退字数估算
        assert_eq!(diff_summary("a\na", "a\na\na"), "+2 字");
        assert_eq!(diff_summary("same", "same"), "未改动");
    }
}
