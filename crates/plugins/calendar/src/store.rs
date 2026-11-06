//! 日程真源读写：本地仓库 `.atelyx/calendar.json`、协作空间团队 meta `calendar` 键。
//! 读失败绝不降级为空清单；损坏的本地原文按内容指纹另存备份，本体保留不动。

use serde::{Deserialize, Serialize};

use atelyx_plugin_protocol::method::{CalendarItem, CollabMetaEntry};
use atelyx_plugin_sdk::Context;

/// 本地仓库的日程文件（仓库相对路径）。
pub const CALENDAR_FILE: &str = ".atelyx/calendar.json";
/// 协作空间团队 meta 的日程键。
pub const META_KEY: &str = "calendar";
/// 日程文件 schema（读侧不校验，仅写盘落字段）。
const SCHEMA: &str = "atelyx-calendar/v1";
/// 备份名里的内容指纹长度（FNV-1a 64 位的十六进制表示）。
const FINGERPRINT_HEX: usize = 16;
/// 空文件不是「没有日程」：可能是同步未完成，既不删也不写。
const EMPTY_NOTICE: &str = "日程文件为空（可能同步未完成），已跳过且未改动原文件";

/// 当前容器的存储去向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Local,
    Space { can_write: bool },
}

#[derive(Serialize, Deserialize, Default)]
struct CalendarFile {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    items: Vec<CalendarItem>,
}

/// 原文的解读结果。
enum Decoded {
    Loaded(Vec<CalendarItem>),
    Empty,
    Corrupt(String),
}

/// 展示用读取：缺失 = 空清单；空文件与损坏都向上传播（不把异常当空清单展示）。
pub async fn load(ctx: &Context, backend: &Backend) -> Result<Vec<CalendarItem>, String> {
    match backend {
        Backend::Local => load_local(ctx).await,
        Backend::Space { .. } => {
            let meta = ctx
                .collab_meta_get(Some(META_KEY))
                .await
                .map_err(|e| format!("读取日程失败：{e}"))?;
            match meta.values.get(META_KEY) {
                Some(raw) => decode_space(raw),
                None => Ok(Vec::new()),
            }
        }
    }
}

/// 写前读取：与展示读取同一口径——损坏与本地空文件都返回 Err，读改写链不得把它们当空清单覆盖。
/// 本地损坏时会把原文另存为内容指纹备份（本体保留），调用方拿到的 Err 即用户可见原因。
pub async fn load_for_write(ctx: &Context, backend: &Backend) -> Result<Vec<CalendarItem>, String> {
    load(ctx, backend).await
}

pub async fn save(ctx: &Context, backend: &Backend, items: &[CalendarItem]) -> Result<(), String> {
    let raw = calendar_json(items)?;
    match backend {
        Backend::Local => ctx.vault_write_text(CALENDAR_FILE, &raw).await.map_err(|e| e.to_string()),
        Backend::Space { .. } => ctx
            .collab_meta_patch(vec![CollabMetaEntry { key: META_KEY.into(), value: raw }])
            .await
            .map_err(|e| e.to_string()),
    }
}

async fn load_local(ctx: &Context) -> Result<Vec<CalendarItem>, String> {
    let exists = ctx
        .vault_exists(CALENDAR_FILE)
        .await
        .map_err(|e| format!("读取日程文件失败：{e}"))?;
    if !exists {
        return Ok(Vec::new());
    }
    let raw = ctx
        .vault_read_text(CALENDAR_FILE)
        .await
        .map_err(|e| format!("读取日程文件失败：{e}"))?;
    match decode(&raw) {
        Decoded::Loaded(items) => Ok(items),
        Decoded::Empty => Err(EMPTY_NOTICE.to_string()),
        Decoded::Corrupt(error) => Err(keep_corrupt(ctx, &raw, &error).await),
    }
}

/// 损坏原文另存为内容指纹备份；本体保留不动（下一次写入由读侧的 Err 挡住）。
async fn keep_corrupt(ctx: &Context, raw: &str, error: &str) -> String {
    let backup = backup_path(raw);
    match ctx.vault_exists(&backup).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(write) = ctx.vault_write_text(&backup, raw).await {
                return format!("日程文件损坏且原文备份失败（{backup}）：{write}；原文件未改动：{error}");
            }
        }
        Err(check) => {
            return format!("日程文件损坏，检查备份是否存在失败（{backup}）：{check}；原文件未改动：{error}");
        }
    }
    format!("日程文件损坏，原文已备份为 {backup}（原文件未改动），修复后重试：{error}")
}

/// 备份路径：内容指纹固定，同一份损坏内容反复读到只留一个备份。
fn backup_path(raw: &str) -> String {
    format!("{CALENDAR_FILE}.corrupt-{}", fingerprint(raw))
}

/// 原文的 FNV-1a 64 位指纹：纯字节遍历，同内容恒定得到同一个备份名。
fn fingerprint(raw: &str) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in raw.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:0width$x}", width = FINGERPRINT_HEX)
}

/// 本地原文：先剥掉开头单个 BOM（部分编辑器/同步工具会写进去）。
fn decode(raw: &str) -> Decoded {
    let body = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if body.trim().is_empty() {
        return Decoded::Empty;
    }
    match serde_json::from_str::<CalendarFile>(body) {
        Ok(file) => Decoded::Loaded(file.items),
        Err(error) => Decoded::Corrupt(error.to_string()),
    }
}

/// 协作空间 meta 值：同一个 JSON 口径，损坏直接报错。
/// 空值按空清单处理——空间 meta 由服务端整体写入，没有本地文件那种同步半成品。
fn decode_space(raw: &str) -> Result<Vec<CalendarItem>, String> {
    match decode(raw) {
        Decoded::Loaded(items) => Ok(items),
        Decoded::Empty => Ok(Vec::new()),
        Decoded::Corrupt(error) => Err(format!("日程数据损坏：{error}")),
    }
}

fn calendar_json(items: &[CalendarItem]) -> Result<String, String> {
    serde_json::to_string(&CalendarFile { schema: SCHEMA.into(), items: items.to_vec() })
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> &'static str {
        r#"{"schema":"atelyx-calendar/v1","items":[{"id":"a","date":"2026-10-01","title":"写方案"}]}"#
    }

    /// 同步工具会往文件头写 BOM，读侧必须先剥掉再解析。
    #[test]
    fn bom_prefixed_file_loads() {
        let raw = format!("\u{feff}{}", sample());
        match decode(&raw) {
            Decoded::Loaded(items) => assert_eq!(items.len(), 1),
            _ => panic!("带 BOM 的日程文件应正常读出"),
        }
    }

    /// 空文件是「同步未完成」而不是「没有日程」：报错交给上层，不删不写。
    #[test]
    fn empty_file_is_reported_as_incomplete_sync() {
        assert!(matches!(decode(""), Decoded::Empty));
        assert!(matches!(decode("  \n\t"), Decoded::Empty));
        assert!(matches!(decode("\u{feff}"), Decoded::Empty));
        // 空间 meta 由服务端整体写入，没有同步半成品：空值就是空清单
        assert!(decode_space("").unwrap().is_empty());
    }

    /// 截断的 JSON 必须报损坏，不能被当成空清单放过去。
    #[test]
    fn truncated_file_is_corrupt_not_empty() {
        let raw = r#"{"schema":"atelyx-calendar/v1","items":[{"id":"a","date":"2026-10-01","tit"#;
        assert!(matches!(decode(raw), Decoded::Corrupt(_)));
        assert!(decode_space(raw).is_err());
    }

    /// 备份名只由内容决定：同内容反复读到只留一个备份，不同内容不撞名。
    #[test]
    fn corrupt_backup_name_is_content_stable() {
        let first = backup_path("坏内容");
        assert_eq!(first, backup_path("坏内容"), "同一份内容必须得到同一个备份名");
        assert_ne!(first, backup_path("另一种坏内容"));
        let name = first.rsplit('/').next().unwrap();
        let hex = name.strip_prefix("calendar.json.corrupt-").expect("备份名带指纹前缀");
        assert_eq!(hex.len(), FINGERPRINT_HEX);
        assert!(hex.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(first, format!(".atelyx/calendar.json.corrupt-{hex}"), "备份与本体同目录");
    }

    /// 指纹必须是 FNV-1a 64 位（取公开参考向量）；偏离该算法会让备份名与契约不符。
    #[test]
    fn fingerprint_matches_fnv1a_64_reference() {
        assert_eq!(fingerprint(""), "cbf29ce484222325");
        assert_eq!(fingerprint("a"), "af63dc4c8601ec8c");
        assert_eq!(fingerprint("foobar"), "85944171f73967e8");
    }

    /// 写盘产物能被自己读回（字段不丢）。
    #[test]
    fn calendar_json_round_trips() {
        let items = vec![
            CalendarItem {
                id: "a".into(),
                date: "2026-10-01".into(),
                title: "写方案".into(),
                note: Some("附注".into()),
                color: Some("#ff0000".into()),
                created_at: 7,
            },
            CalendarItem {
                id: "b".into(),
                date: "2026-10-02".into(),
                title: "评审".into(),
                note: None,
                color: None,
                created_at: 8,
            },
        ];
        let raw = calendar_json(&items).expect("序列化日程");
        match decode(&raw) {
            Decoded::Loaded(read) => {
                assert_eq!(read.len(), 2);
                assert_eq!(read[0].note.as_deref(), Some("附注"));
                assert_eq!(read[1].title, "评审");
            }
            _ => panic!("写盘产物应能读回"),
        }
    }

    /// 存量文件缺字段走默认值，不因少一个键判损坏。
    #[test]
    fn missing_fields_fall_back_to_defaults() {
        match decode(r#"{"items":[{"id":"a","date":"2026-10-01","title":"t"}]}"#) {
            Decoded::Loaded(items) => {
                assert_eq!(items.len(), 1);
                assert!(items[0].note.is_none());
            }
            _ => panic!("缺 schema 字段不应判损坏"),
        }
        assert!(matches!(decode("{}"), Decoded::Loaded(items) if items.is_empty()));
    }
}

