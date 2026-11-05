//! 界面重建计量（仅调试构建）：设 `ATELYX_UI_METRICS=1` 时每秒打印 update 与 view 次数。

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(debug_assertions)]
use std::sync::{Mutex, OnceLock};
#[cfg(debug_assertions)]
use std::time::{Duration, Instant};

#[cfg(debug_assertions)]
static ENABLED: OnceLock<bool> = OnceLock::new();
#[cfg(debug_assertions)]
static UPDATES: AtomicU64 = AtomicU64::new(0);
#[cfg(debug_assertions)]
static VIEWS: AtomicU64 = AtomicU64::new(0);
#[cfg(debug_assertions)]
static REPORT: OnceLock<Mutex<(Instant, u64, u64)>> = OnceLock::new();

/// 计一条 update 消息，并按秒打印累计速率。
#[inline]
pub(crate) fn count_update() {
    #[cfg(debug_assertions)]
    {
        if !*ENABLED.get_or_init(|| std::env::var_os("ATELYX_UI_METRICS").is_some()) {
            return;
        }
        let updates = UPDATES.fetch_add(1, Ordering::Relaxed) + 1;
        let views = VIEWS.load(Ordering::Relaxed);
        let mut report = REPORT
            .get_or_init(|| Mutex::new((Instant::now(), 0, 0)))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if report.0.elapsed() < Duration::from_secs(1) {
            return;
        }
        let (update_rate, view_rate) = (updates - report.1, views - report.2);
        *report = (Instant::now(), updates, views);
        eprintln!("ui-metrics: update={update_rate}/s view={view_rate}/s");
    }
}

/// 计一次 `App::view()` 调用（iced 对每个窗口各调一次）。
#[inline]
pub(crate) fn count_view() {
    #[cfg(debug_assertions)]
    if *ENABLED.get_or_init(|| std::env::var_os("ATELYX_UI_METRICS").is_some()) {
        VIEWS.fetch_add(1, Ordering::Relaxed);
    }
}
