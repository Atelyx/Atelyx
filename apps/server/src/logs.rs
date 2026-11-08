//! 服务日志采集：把 tracing 事件收进进程内环形缓冲，供管理台「服务日志」面板读取；
//! 日志只记元数据不记内容（密码 / 令牌 / 正文一律不入日志）。
//! 缓冲是进程级资源（日志属于进程而非某个 `ServerState`），容量固定、写满覆盖最旧，内存有界，重启清空。
//! 采集范围由订阅器的 `RUST_LOG` 决定，本模块不二次过滤（面板看不到 debug 只说明服务端没开到 debug 级别）。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// 环形缓冲容量（条）。
const CAPACITY: usize = 2000;
/// 单次查询默认返回条数。
pub const DEFAULT_LIMIT: usize = 300;
/// 单次查询返回上限 = 缓冲容量：一次请求即可取回缓冲内全部条目，
/// 调用方游标落后（中间条目已被覆盖）时得以整窗重建，不残留空档。
pub const MAX_LIMIT: usize = CAPACITY;
/// 新条目通知的订阅端容量：只作唤醒信号（不承载内容），订阅端落后时按游标从缓冲补取。
const NOTIFY_CAPACITY: usize = 1;

/// 级别名（小写，与过滤参数同形），下标即级别高低。
const LEVEL_NAMES: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// 一条日志。`fields` 为消息之外的结构化字段（`key=value` 以空格连接），无字段时为空串。
#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub seq: u64,
    /// unix 毫秒。
    pub ts: i64,
    pub level: &'static str,
    pub target: String,
    pub message: String,
    pub fields: String,
}

/// 一次读取的窗口。`latest` = 缓冲内最新序号，`first` = 缓冲内最旧序号
/// （调用方游标早于 `first` 说明其间条目已被覆盖，需整窗重取）。
pub struct LogPage {
    pub latest: u64,
    pub first: u64,
    pub entries: Vec<LogEntry>,
}

struct Store {
    entries: Mutex<VecDeque<LogEntry>>,
    /// 新条目唤醒信号（实时读取端订阅；内容仍从 `entries` 按游标取）。
    notify: broadcast::Sender<()>,
}

fn store() -> &'static Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE.get_or_init(|| {
        let (notify, _) = broadcast::channel(NOTIFY_CAPACITY);
        Store { entries: Mutex::new(VecDeque::with_capacity(CAPACITY)), notify }
    })
}

/// 订阅新条目信号。订阅端收到信号后用 `query` 从缓冲按游标取条目——
/// 信号可能合并（落后时只报一次唤醒），条目本身不丢。
pub fn subscribe() -> broadcast::Receiver<()> {
    store().notify.subscribe()
}

/// 级别名 → 级别序号；不区分大小写，非法取值返回 None。
pub fn level_index(name: &str) -> Option<usize> {
    let lower = name.to_ascii_lowercase();
    LEVEL_NAMES.iter().position(|n| *n == lower)
}

/// 采集层：挂到订阅器上即生效（`main.rs` 与控制台日志并列输出，互不影响）。
pub fn layer() -> LogLayer {
    LogLayer
}

pub struct LogLayer;

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldsVisitor::default();
        event.record(&mut visitor);
        push(
            level_name(event.metadata().level()),
            event.metadata().target(),
            visitor.message.unwrap_or_default(),
            visitor
                .fields
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
}

/// 追加一条（写满即丢最旧）。序号在临界区内按上一条递增分配——先取号后入队会因线程
/// 调度倒挂，使 `entries` 不再按序号升序，游标增量读取随之错乱。
fn push(level: &'static str, target: &str, message: String, fields: String) {
    let store = store();
    let ts = now_millis();
    let mut entries = store.entries.lock().unwrap();
    let seq = entries.back().map(|e| e.seq + 1).unwrap_or(1);
    if entries.len() == CAPACITY {
        entries.pop_front();
    }
    entries.push_back(LogEntry { seq, ts, level, target: target.to_string(), message, fields });
    drop(entries);
    // 无订阅者时 send 报错，属正常情形（无人看实时日志）
    let _ = store.notify.send(());
}

/// 读取日志窗口。
///
/// - `after` 为 None：取最新的若干条（首屏 / 整窗刷新）；
/// - `after` 为 Some(seq)：取该序号之后最早的若干条（轮询增量；未被本次窗口取走的条目
///   仍在缓冲内，下次轮询继续取，不丢条）。
/// - `min_level` 为级别下限序号，低于它的条目不返回。
pub fn query(after: Option<u64>, min_level: usize, limit: usize) -> LogPage {
    let store = store();
    let entries = store.entries.lock().unwrap();
    let latest = entries.back().map(|e| e.seq).unwrap_or(0);
    let first = entries.front().map(|e| e.seq).unwrap_or(0);
    let matched = |e: &LogEntry| rank(e.level) >= min_level;
    let picked: Vec<LogEntry> = match after {
        Some(cursor) => entries
            .iter()
            .filter(|e| e.seq > cursor && matched(e))
            .take(limit)
            .cloned()
            .collect(),
        None => {
            let mut taken: Vec<LogEntry> =
                entries.iter().rev().filter(|e| matched(e)).take(limit).cloned().collect();
            taken.reverse();
            taken
        }
    };
    LogPage { latest, first, entries: picked }
}

fn rank(level: &str) -> usize {
    level_index(level).unwrap_or(0)
}

fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::TRACE => "trace",
        Level::DEBUG => "debug",
        Level::INFO => "info",
        Level::WARN => "warn",
        Level::ERROR => "error",
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// 字段收集：`message` 单独存放，其余按 `key=value` 保留出现顺序。
/// `record_str` 显式实现，字符串值不套 Debug 引号（`username=alice` 而非 `username="alice"`）。
#[derive(Default)]
struct FieldsVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl FieldsVisitor {
    fn put(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl Visit for FieldsVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// 采集缓冲是进程级共享资源：本模块测试串行执行，断言才不受其他测试写入的条目干扰。
    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 采集层把事件写进缓冲：级别、目标、消息与结构化字段都可读到。
    #[test]
    fn captures_event_with_fields() {
        let _guard = test_guard();
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "capture-1", space_id = "sp1", bytes = 12, "内容写入");
        });
        let page = query(None, 0, MAX_LIMIT);
        let entry = page
            .entries
            .iter()
            .find(|e| e.fields.contains("marker=capture-1"))
            .expect("应采集到刚写入的事件");
        assert_eq!(entry.message, "内容写入");
        assert_eq!(entry.level, "info");
        assert_eq!(entry.fields, "marker=capture-1 space_id=sp1 bytes=12");
        assert!(entry.target.starts_with("atelyx_server::logs"), "目标应为事件所在模块：{}", entry.target);
        assert!(entry.ts > 0);
    }

    /// 级别下限过滤：低于下限的条目不返回，达到下限的返回。
    #[test]
    fn filters_below_min_level() {
        let _guard = test_guard();
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "level-info-1", "低级别事件");
            tracing::warn!(marker = "level-warn-1", "高级别事件");
        });
        let has = |min: &str, marker: &str| {
            let min = level_index(min).unwrap();
            query(None, min, MAX_LIMIT).entries.iter().any(|e| e.fields.contains(marker))
        };
        assert!(has("info", "level-info-1"), "info 下限应含 info 事件");
        assert!(!has("warn", "level-info-1"), "warn 下限应滤掉 info 事件");
        assert!(has("warn", "level-warn-1"), "warn 下限应含 warn 事件");
    }

    /// 增量读取：`after` 只返回其后的条目，全部取走后游标即最新序号。
    #[test]
    fn incremental_query_returns_only_new_entries() {
        let _guard = test_guard();
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(marker = "inc-a", "第一条");
        });
        let cursor = query(None, 0, MAX_LIMIT).latest;
        let page = query(Some(cursor), 0, MAX_LIMIT);
        assert!(page.entries.is_empty(), "游标之后不应有历史条目");
        assert_eq!(page.latest, cursor);

        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer()), || {
            tracing::error!(marker = "inc-b", "第二条");
        });
        let page = query(Some(cursor), 0, MAX_LIMIT);
        assert_eq!(page.entries.len(), 1, "只应返回新条目：{:?}", page.entries.len());
        assert_eq!(page.entries[0].level, "error");
        assert_eq!(page.entries[0].seq, cursor + 1);
        assert_eq!(page.latest, cursor + 1);
    }

    /// 增量读取按「游标之后最早的一批」返回（不是最新一批）——实时流据此分批推进而不跳条。
    #[test]
    fn incremental_query_takes_oldest_after_cursor() {
        let _guard = test_guard();
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            for i in 0..5 {
                tracing::info!(marker = "batch", idx = i, "批量事件");
            }
        });
        let first_batch_seq = query(None, 0, MAX_LIMIT)
            .entries
            .iter()
            .find(|e| e.fields.contains("marker=batch"))
            .expect("应采集到批量事件")
            .seq;

        let page = query(Some(first_batch_seq - 1), 0, 3);
        assert_eq!(page.entries.len(), 3, "应返回游标之后最早的 3 条");
        assert_eq!(page.entries[0].seq, first_batch_seq, "应从游标之后第一条开始");

        let rest = query(Some(page.entries[2].seq), 0, 3);
        assert_eq!(rest.entries.len(), 2, "继续取应拿到剩余 2 条");
        assert!(rest.entries.iter().all(|e| e.seq > page.entries[2].seq));
    }

    /// 级别名解析：忽略大小写，非法取值拒绝。
    #[test]
    fn parses_level_names() {
        assert_eq!(level_index("WARN"), Some(3));
        assert_eq!(level_index("trace"), Some(0));
        assert_eq!(level_index("bogus"), None);
    }
}