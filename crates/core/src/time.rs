//! 时间基元：进程内统一的时间戳出口（UNIX_EPOCH 起算，取不到时钟按 0）。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 Unix 秒。
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 当前 Unix 毫秒。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_and_millis_agree() {
        let secs = now_secs();
        let ms = now_ms();
        assert!(secs > 1_600_000_000, "取到的是 2020 年之后的真实时钟：{secs}");
        assert!((ms / 1000 - secs).abs() <= 1, "两种粒度同一时刻：{secs} vs {ms}");
    }
}
