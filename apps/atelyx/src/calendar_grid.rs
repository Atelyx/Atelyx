//! 日历日期数学与按日归组：civil 日期与自纪元天数互转、月网格推导、
//! 历史条目与活动计数按本地日界归组（纯函数，无 I/O 与 GUI 依赖）。

use std::collections::HashMap;

use atelyx_core::date::{day_key, days_from_civil, days_in_month, month_add, weekday_mon0};
use atelyx_core::history::{HistoryAction, HistoryKind};

use crate::repohistory_view::RepoHistoryGroup;

/// 月网格：7 列整周铺开（周一起始），当月天按序，其余 None；
/// 长度为 7 的倍数，首个格子 = 1 日所在周的周一。
pub fn month_cells(y: i64, m: u32) -> Vec<Option<u32>> {
    let last = days_in_month(y, m);
    let offset = weekday_mon0(days_from_civil(y, m, 1)) as i64;
    let weeks = ((offset + last as i64) + 6) / 7;
    (0..weeks * 7)
        .map(|i| {
            let day = i - offset + 1;
            (1..=last as i64).contains(&day).then_some(day as u32)
        })
        .collect()
}

/// 热力图月份窗口：以 (base_y, base_m) 为中心的 `months` 个月（旧→新；`months` 为奇数）。
pub fn month_window_around(base_y: i64, base_m: u32, months: u32) -> Vec<(i64, u32)> {
    let half = months.saturating_sub(1) as i32 / 2;
    (0..months as i32)
        .map(|i| month_add(base_y, base_m, i - half))
        .collect()
}

/// 月份标签（一月带年份，防跨年窗口混淆）。
pub fn month_label(y: i64, m: u32) -> String {
    if m == 1 {
        format!("{y}年1月")
    } else {
        format!("{m}月")
    }
}

/// 当日历史条目（来自仓库历史缓存，新→旧）。
pub struct DayEntry {
    pub ts: i64,
    pub kind: HistoryKind,
    pub file: String,
    pub action: HistoryAction,
    pub author_name: String,
    pub summary: Option<String>,
    /// AI 编辑来源（None = 人类编辑）。
    pub editor: Option<atelyx_core::history::HistoryEditor>,
}

/// 历史聚合版本按本地日界过滤（ts 毫秒；day = 自纪元天数）。
pub fn entries_on_day(groups: &[RepoHistoryGroup], day: i64) -> Vec<DayEntry> {
    let mut out: Vec<DayEntry> = groups
        .iter()
        .flat_map(|g| {
            g.versions
                .iter()
                .filter(|v| day_key(v.ts) == day)
                .map(move |v| DayEntry {
                    ts: v.ts,
                    kind: g.kind,
                    file: g.file.clone(),
                    action: v.action,
                    author_name: v.author_name.clone(),
                    summary: v.summary.clone(),
                    editor: v.editor.clone(),
                })
        })
        .collect();
    out.sort_by_key(|v| std::cmp::Reverse(v.ts));
    out
}

/// 活动密度计数（服务端聚合版本按本地日界计数；本地仓库走 core `activity_by_day`）。
pub fn activity_from_groups(groups: &[RepoHistoryGroup]) -> HashMap<i64, u32> {
    let mut counts: HashMap<i64, u32> = HashMap::new();
    for g in groups {
        for v in &g.versions {
            *counts.entry(day_key(v.ts)).or_insert(0) += 1;
        }
    }
    counts
}

/// 热力图色阶档位：0 = 无活动，1-3 / 4-7 / 8+ 三档。
pub fn activity_level(count: u32) -> u8 {
    match count {
        0 => 0,
        1..=3 => 1,
        4..=7 => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_cells_pad_full_weeks_monday_start() {
        // 2026-10：1 日为周四（偏移 3），31 天 → 5 周 35 格，首两格与末格留空
        let cells = month_cells(2026, 10);
        assert_eq!(cells.len(), 35);
        assert_eq!(cells[..3], [None, None, None]);
        assert_eq!(cells[3], Some(1));
        assert_eq!(cells[33], Some(31));
        assert_eq!(cells[34], None);
        // 2026-02：2 月 1 日为周日（偏移 6），28 天 → 5 周，日 28 落在末周周六
        let cells = month_cells(2026, 2);
        assert_eq!(cells.len(), 35);
        assert_eq!(cells[6], Some(1));
        assert_eq!(cells[33], Some(28));
        assert_eq!(cells[34], None);
        // 整周恰好收尾（2024-02 闰月：1 日周四 + 29 天 = 5 周）
        assert_eq!(month_cells(2024, 2).len(), 35);
        // 全月天恰好出现一次
        let days: Vec<u32> = month_cells(2026, 10).into_iter().flatten().collect();
        assert_eq!(days, (1..=31).collect::<Vec<_>>());
    }

    #[test]
    fn month_add_and_window_cross_years() {
        assert_eq!(month_add(2026, 1, -1), (2025, 12));
        assert_eq!(month_add(2025, 12, 1), (2026, 1));
        assert_eq!(month_add(2026, 6, -6), (2025, 12));
        assert_eq!(month_add(2026, 7, 12), (2027, 7));
        let window = month_window_around(2026, 3, 13);
        assert_eq!(window.first(), Some(&(2025, 9)));
        assert_eq!(window.last(), Some(&(2026, 9)));
        assert_eq!(window.len(), 13);
        assert_eq!(window[6], (2026, 3));
        assert_eq!(month_window_around(2026, 1, 3), [(2025, 12), (2026, 1), (2026, 2)]);
        assert_eq!(month_window_around(2026, 1, 1), [(2026, 1)]);
    }

    #[test]
    fn entries_on_day_filters_and_sorts_newest_first() {
        let group = |file: &str, ts: i64| RepoHistoryGroup {
            kind: HistoryKind::Note,
            file: file.into(),
            versions: vec![crate::repohistory_view::RepoVersionRow {
                ts,
                action: HistoryAction::Edit,
                author_name: "a".into(),
                summary: None,
                editor: None,
            }],
        };
        // 相邻整天（86_400_000 ms 间隔，环境时区无关：不同整天必不同日）
        let (t1, t2, t3) = (5_000_000_000, 5_086_400_000, 5_172_800_000);
        let groups = vec![
            group("a.md", t1),
            group("b.md", t2),
            group("b2.md", t2 + 60_000),
            group("c.md", t3),
        ];
        // 当日两条按时间新→旧
        let entries = entries_on_day(&groups, day_key(t2));
        assert_eq!(
            entries.iter().map(|e| e.file.as_str()).collect::<Vec<_>>(),
            ["b2.md", "b.md"]
        );
        assert_eq!(entries_on_day(&groups, day_key(t1))[0].file, "a.md");
        assert!(entries_on_day(&groups, day_key(t1) - 2).is_empty());
    }

    #[test]
    fn activity_counts_and_levels() {
        let group = |ts: i64| RepoHistoryGroup {
            kind: HistoryKind::Note,
            file: "a.md".into(),
            versions: vec![crate::repohistory_view::RepoVersionRow {
                ts,
                action: HistoryAction::Edit,
                author_name: "a".into(),
                summary: None,
                editor: None,
            }],
        };
        let counts = activity_from_groups(&[group(5_000), group(6_000), group(90_000_000)]);
        assert_eq!(counts.get(&day_key(5_000)), Some(&2));
        assert_eq!(counts.get(&day_key(90_000_000)), Some(&1));
        assert_eq!(activity_level(0), 0);
        assert_eq!(activity_level(3), 1);
        assert_eq!(activity_level(4), 2);
        assert_eq!(activity_level(7), 2);
        assert_eq!(activity_level(8), 3);
    }
}
