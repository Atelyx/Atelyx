//! 日历日期数学与按日归组：civil 日期与自纪元天数互转、月网格推导、
//! 历史条目与活动计数按本地日界归组（纯函数，无 I/O 与 GUI 依赖）。

use std::collections::HashMap;

use atelyx_core::history::{HistoryAction, HistoryKind, day_key};

use crate::repohistory_view::RepoHistoryGroup;

/// 平年各月天数（闰二月另判）。
const MONTH_DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// 自纪元天数 → (年, 月, 日)（proleptic Gregorian civil 算法）。
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// (年, 月, 日) → 自纪元天数（civil_from_days 逆变换）。
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 星期序（周一 = 0 … 周日 = 6；1970-01-01 为周四）。
pub fn weekday_mon0(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

/// 闰年判定。
fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// 当月天数。
pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        1..=12 => MONTH_DAYS[(m - 1) as usize],
        _ => 0,
    }
}

/// 月份加减 n（n 可负，跨年归一）。
pub fn month_add(y: i64, m: u32, n: i32) -> (i64, u32) {
    let total = y * 12 + m as i64 - 1 + n as i64;
    (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
}

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

/// 日期字符串 `YYYY-MM-DD` → 自纪元天数（非法值 None；读侧不滤非法 date，展示层跳过）。
pub fn parse_date(s: &str) -> Option<i64> {
    let (y, rest) = s.split_once('-')?;
    let (m, d) = rest.split_once('-')?;
    let (y, m, d) = (y.parse::<i64>().ok()?, m.parse::<u32>().ok()?, d.parse::<u32>().ok()?);
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// 自纪元天数 → `YYYY-MM-DD`（CalendarItem.date 的存储格式）。
pub fn date_string(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 当日历史条目（来自仓库历史缓存，新→旧）。
pub struct DayEntry {
    pub ts: i64,
    pub kind: HistoryKind,
    pub file: String,
    pub action: HistoryAction,
    pub author_name: String,
    pub summary: Option<String>,
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
    fn civil_round_trip_and_anchors() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 世纪锚点
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
        // 跨年相邻
        assert_eq!(days_from_civil(2026, 1, 1) - days_from_civil(2025, 12, 31), 1);
        // 往返覆盖闰年边界附近
        for days in [86_399_i64 * 10, -1, 1, 19_000, 20_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn leap_year_february_lengths() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 10), 31);
        assert_eq!(days_in_month(2026, 4), 30);
    }

    #[test]
    fn weekday_is_monday_based() {
        // 1970-01-01 = 周四
        assert_eq!(weekday_mon0(0), 3);
        // 2026-01-01 = 周四；2026-10-01 = 周四；2026-10-13（任务当日）= 周二
        assert_eq!(weekday_mon0(days_from_civil(2026, 1, 1)), 3);
        assert_eq!(weekday_mon0(days_from_civil(2026, 10, 1)), 3);
        assert_eq!(weekday_mon0(days_from_civil(2026, 10, 13)), 1);
    }

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
    fn date_parse_and_format_round_trip() {
        assert_eq!(date_string(days_from_civil(2026, 10, 13)), "2026-10-13");
        assert_eq!(date_string(days_from_civil(2024, 2, 29)), "2024-02-29");
        assert_eq!(parse_date("2026-10-13"), Some(days_from_civil(2026, 10, 13)));
        // 闰年 2-29 合法、平年 2-29 非法、月份与日越界拒绝
        assert!(parse_date("2024-02-29").is_some());
        assert_eq!(parse_date("2026-02-29"), None);
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("2026-10-00"), None);
        assert_eq!(parse_date("garbage"), None);
        assert_eq!(parse_date(""), None);
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
