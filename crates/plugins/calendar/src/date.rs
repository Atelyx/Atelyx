//! 日历日期数学：公历与自纪元天数互转、月网格推导与本地时间换算。
//! 纯公历；本地日界取系统时区，与宿主历史归日同一口径。

use chrono::{Datelike, Local, TimeZone, Timelike};

/// 平年各月天数（闰二月另判）。
const MONTH_DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// 今天的本地日（自纪元天数）。
pub fn today() -> i64 {
    let date = Local::now().date_naive();
    days_from_civil(date.year() as i64, date.month(), date.day())
}

/// 本地当下的 Unix 毫秒（新建日程的创建时刻）。
pub fn now_ms() -> i64 {
    Local::now().timestamp_millis()
}

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

/// (年, 月, 日) → 自纪元天数。
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

/// 月网格：整周铺开（周一起始），当月天按序，其余 `None`；长度按实际周数（4–6 周）。
pub fn month_cells(y: i64, m: u32) -> Vec<Option<u32>> {
    let last = days_in_month(y, m);
    let offset = weekday_mon0(days_from_civil(y, m, 1)) as i64;
    let weeks = (offset + last as i64 + 6) / 7;
    (0..weeks * 7)
        .map(|i| {
            let day = i - offset + 1;
            (1..=last as i64).contains(&day).then_some(day as u32)
        })
        .collect()
}

/// 以 (base_y, base_m) 为中心的 `months` 个月（旧→新；调用方传奇数，导航月居中）。
pub fn month_window_around(base_y: i64, base_m: u32, months: u32) -> Vec<(i64, u32)> {
    let half = months.saturating_sub(1) as i32 / 2;
    (0..months as i32)
        .map(|i| month_add(base_y, base_m, i - half))
        .collect()
}

/// `YYYY-MM-DD` → 自纪元天数（形态或范围非法 = `None`）。
pub fn parse_date(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    if !shaped {
        return None;
    }
    let (y, rest) = s.split_once('-')?;
    let (m, d) = rest.split_once('-')?;
    let (y, m, d) = (y.parse::<i64>().ok()?, m.parse::<u32>().ok()?, d.parse::<u32>().ok()?);
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// 自纪元天数 → `YYYY-MM-DD`（日程 `date` 字段的存储格式）。
pub fn date_string(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 热力图月块标签（一月带年份，防跨年窗口混淆）。
pub fn month_label(y: i64, m: u32) -> String {
    if m == 1 {
        format!("{y}年1月")
    } else {
        format!("{m}月")
    }
}

/// 选中日的标题（如 `10月29日`）。
pub fn day_label(days: i64) -> String {
    let (_, m, d) = civil_from_days(days);
    format!("{m}月{d}日")
}

/// Unix 毫秒 → 本地 `(时, 分)`；超出可表示范围的时刻按 `(0, 0)`。
pub fn time_hm(ts_ms: i64) -> (u32, u32) {
    match Local.timestamp_millis_opt(ts_ms).single() {
        Some(time) => (time.hour(), time.minute()),
        None => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        for days in [-1_000_000, -1, 0, 1, 20_000, 1_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn month_grid_starts_on_monday_and_covers_the_month() {
        // 2026-10-01 是周四：网格首格为其所在周的周一
        let cells = month_cells(2026, 10);
        assert_eq!(cells.len() % 7, 0);
        assert_eq!(cells[0], None);
        assert_eq!(weekday_mon0(days_from_civil(2026, 10, 1)), 3);
        assert_eq!(cells[3], Some(1));
        assert_eq!(cells[3 + 30], Some(31));
        assert!(month_cells(2026, 2).contains(&Some(28)));
        assert!(month_cells(2024, 2).contains(&Some(29)), "闰年二月");
    }

    #[test]
    fn parse_and_format_dates() {
        assert_eq!(parse_date("2026-10-29"), Some(days_from_civil(2026, 10, 29)));
        assert_eq!(date_string(days_from_civil(2026, 10, 5)), "2026-10-05");
        for bad in ["2026-13-01", "2026-02-30", "2026-1-1", "", "abc"] {
            assert_eq!(parse_date(bad), None, "应拒绝：{bad}");
        }
    }

    #[test]
    fn month_add_crosses_years() {
        assert_eq!(month_add(2026, 12, 1), (2027, 1));
        assert_eq!(month_add(2026, 1, -1), (2025, 12));
        assert_eq!(month_add(2026, 6, -18), (2024, 12));
    }

    #[test]
    fn month_window_is_centered_and_ordered() {
        assert_eq!(
            month_window_around(2026, 10, 3),
            [(2026, 9), (2026, 10), (2026, 11)]
        );
        let window = month_window_around(2026, 1, 13);
        assert_eq!(window.len(), 13);
        assert_eq!(window[6], (2026, 1), "导航月居中");
        assert_eq!(window[0], (2025, 7));
        assert_eq!(window[12], (2026, 7));
    }

    #[test]
    fn labels_carry_the_year_only_when_needed() {
        assert_eq!(month_label(2027, 1), "2027年1月");
        assert_eq!(month_label(2026, 10), "10月");
        assert_eq!(day_label(days_from_civil(2026, 10, 29)), "10月29日");
    }

    #[test]
    fn local_time_uses_the_machine_zone() {
        let stamp = Local
            .with_ymd_and_hms(2026, 10, 30, 9, 5, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        assert_eq!(time_hm(stamp), (9, 5));
        assert_eq!(time_hm(i64::MAX), (0, 0), "不可表示的毫秒按零值");
    }
}
