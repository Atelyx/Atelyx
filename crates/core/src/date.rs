//! 日历日与本地时刻的纯换算：civil 日期互转、本地日界、展示格式。

use crate::time::now_ms;

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

/// (年, 月, 日) → 自纪元天数（`civil_from_days` 的逆变换）。
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 闰年判定。
pub fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// 当月天数（月份非法 = 0）。
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

/// 星期序（周一 = 0 … 周日 = 6；1970-01-01 为周四）。
pub fn weekday_mon0(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

/// 自纪元天数 → `YYYY-MM-DD`（日历 `date` 字段的存储格式）。
pub fn day_string(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 本地日历日字符串（`YYYY-MM-DD`）→ 自纪元天数，与 [`day_key`] 同口径。
/// 形状或历法非法（月超 12、2 月 31 日这类）返回 `None`。
pub fn parse_day(value: &str) -> Option<i64> {
    let (year, rest) = value.split_once('-')?;
    let (month, day) = rest.split_once('-')?;
    let digits = |s: &str, len: usize| -> Option<i64> {
        if s.len() != len || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let (year, month, day) = (digits(year, 4)?, digits(month, 2)?, digits(day, 2)?);
    if !(1..=12).contains(&month) {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    // 往返校验吸收日超当月天数（2 月 31 日会进位到下月）
    (civil_from_days(days) == (year, month as u32, day as u32)).then_some(days)
}

/// 当前本地时区相对 UTC 的偏移（毫秒，东正西负）。对全部时间戳统一用当前偏移折算
/// 本地日界（不逐时间戳回算历史时区）；取不到按 UTC。
#[cfg(target_os = "windows")]
pub fn local_offset_ms() -> i64 {
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
pub fn local_offset_ms() -> i64 {
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
pub fn local_offset_ms() -> i64 {
    0
}

/// 毫秒时间戳按 `offset_ms` 时区偏移折算自纪元天数（`offset_ms` 可注入便于测试）。
/// 活动密度计数与展示层按日过滤共用此口径，避免两处实现漂移。
pub fn day_key_with(ts_ms: i64, offset_ms: i64) -> i64 {
    (ts_ms + offset_ms).div_euclid(86_400_000)
}

/// [`day_key_with`] 取当前系统时区偏移。
pub fn day_key(ts_ms: i64) -> i64 {
    day_key_with(ts_ms, local_offset_ms())
}

/// 本地当下（自纪元天数，本地日界）。
pub fn today() -> i64 {
    day_key(now_ms())
}

/// 毫秒时间戳 → 本地时刻 (时, 分)（当日版本行的展示粒度）。
pub fn local_time_hm(ts_ms: i64) -> (u32, u32) {
    let secs = (ts_ms + local_offset_ms()).div_euclid(1000).rem_euclid(86_400);
    ((secs / 3600) as u32, ((secs % 3600) / 60) as u32)
}

/// 毫秒时间戳 → 本地 `YYYY-MM-DD HH:MM`（列表与历史条目的时间列）。
pub fn format_minutes(ms: i64) -> String {
    let secs = (ms + local_offset_ms()).div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs_of_day / 3600, (secs_of_day % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
        assert_eq!(days_from_civil(2026, 1, 1) - days_from_civil(2025, 12, 31), 1);
        for days in -20_000..20_000 {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn month_days_and_weekday() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 10), 31);
        assert_eq!(days_in_month(2026, 13), 0);

        assert_eq!(weekday_mon0(0), 3);
        assert_eq!(weekday_mon0(days_from_civil(2026, 1, 1)), 3);
        assert_eq!(weekday_mon0(days_from_civil(2026, 10, 1)), 3);
        assert_eq!(weekday_mon0(days_from_civil(2026, 10, 13)), 1);
    }

    #[test]
    fn month_add_crosses_years() {
        assert_eq!(month_add(2026, 1, -1), (2025, 12));
        assert_eq!(month_add(2025, 12, 1), (2026, 1));
        assert_eq!(month_add(2026, 6, -6), (2025, 12));
        assert_eq!(month_add(2026, 7, 12), (2027, 7));
    }

    #[test]
    fn day_string_and_parse_round_trip() {
        assert_eq!(day_string(days_from_civil(2026, 10, 13)), "2026-10-13");
        assert_eq!(day_string(days_from_civil(2024, 2, 29)), "2024-02-29");
        assert_eq!(parse_day("2026-10-13"), Some(days_from_civil(2026, 10, 13)));
        assert_eq!(parse_day("1970-01-01"), Some(0));
        assert_eq!(parse_day("2024-02-29"), Some(19_782));
        // 形状或历法非法一律拒绝（日超当月天数不得进位成下月）
        for bad in ["2026-2-30", "2026-13-01", "2026-02-30", "2026-00-10", "2026-10-00", "2026/10/30", "2026-10-30T00:00", "garbage", ""] {
            assert_eq!(parse_day(bad), None, "{bad}");
        }
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
