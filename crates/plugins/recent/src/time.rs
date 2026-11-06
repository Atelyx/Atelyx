//! 时间文案：打开时刻 → 相对时间（刚刚 / N 分钟前 / N 小时前 / N 天前 / 本地日期）。

use chrono::{FixedOffset, Local, Offset, TimeZone};

/// 当前本地时区相对 UTC 的秒偏移（东正西负；与宿主历史展示同一口径）。
fn local_offset_secs() -> i32 {
    Local::now().offset().fix().local_minus_utc()
}

/// 秒级时间戳 → 相对时间文案；30 天及以上退回本地日期（YYYY-MM-DD）。
pub fn relative(secs: i64, now: i64) -> String {
    let d = now - secs;
    if d < 60 {
        return "刚刚".to_string();
    }
    if d < 3_600 {
        return format!("{} 分钟前", d / 60);
    }
    if d < 86_400 {
        return format!("{} 小时前", d / 3_600);
    }
    if d < 30 * 86_400 {
        return format!("{} 天前", d / 86_400);
    }
    local_date(secs)
}

/// 本地日期：按当前时区偏移折算（宿主同口径，跨夏令时边界可能差一天，与宿主一致）。
fn local_date(secs: i64) -> String {
    let Some(offset) = FixedOffset::east_opt(local_offset_secs()) else {
        return String::new();
    };
    match offset.timestamp_opt(secs, 0).single() {
        Some(time) => time.format("%Y-%m-%d").to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_ladder_matches_the_host() {
        let now = 1_000_000_000;
        assert_eq!(relative(now, now), "刚刚");
        assert_eq!(relative(now - 120, now), "2 分钟前");
        assert_eq!(relative(now - 7_200, now), "2 小时前");
        assert_eq!(relative(now - 3 * 86_400, now), "3 天前");
        // 未来时刻按「刚刚」处理（时钟回拨时不出现负数文案）
        assert_eq!(relative(now + 100, now), "刚刚");
        let old = relative(now - 40 * 86_400, now);
        assert_eq!(old.len(), "YYYY-MM-DD".len());
        assert!(old.contains('-'));
    }
}
