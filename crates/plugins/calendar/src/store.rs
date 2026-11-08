//! 日程真源读写：本地仓库 `.atelyx/calendar.json`、协作空间团队 meta `calendar` 键。
//! 读失败绝不降级为空清单；损坏的本地原文先备份再报错，避免下一次写入覆盖用户数据。

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use serde::{Deserialize, Serialize};

use atelyx_plugin_protocol::method::{CalendarItem, CollabMetaEntry};
use atelyx_plugin_sdk::Context;

use crate::date;

/// 本地仓库的日程文件（仓库相对路径）。
pub const CALENDAR_FILE: &str = ".atelyx/calendar.json";
/// 协作空间团队 meta 的日程键。
pub const META_KEY: &str = "calendar";
/// 日程文件 schema（读侧不校验，仅写盘落字段）。
const SCHEMA: &str = "atelyx-calendar/v1";

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

/// 展示用读取：缺失 = 空清单；损坏与其余失败都向上传播（不把损坏当空清单展示）。
pub async fn load(ctx: &Context, backend: &Backend) -> Result<Vec<CalendarItem>, String> {
    match backend {
        Backend::Local => load_local(ctx).await,
        Backend::Space { .. } => {
            let meta = ctx
                .collab_meta_get(Some(META_KEY))
                .await
                .map_err(|e| format!("读取日程失败：{e}"))?;
            match meta.values.get(META_KEY) {
                Some(raw) => parse_strict(raw),
                None => Ok(Vec::new()),
            }
        }
    }
}

/// 写前读取：值损坏 = Err（读改写链不得把损坏内容当作空清单覆盖）。
pub async fn load_for_write(ctx: &Context, backend: &Backend) -> Result<Vec<CalendarItem>, String> {
    match backend {
        Backend::Local => load_local(ctx).await,
        Backend::Space { .. } => {
            let meta = ctx
                .collab_meta_get(Some(META_KEY))
                .await
                .map_err(|e| format!("读取日程失败：{e}"))?;
            match meta.values.get(META_KEY) {
                Some(raw) => parse_strict(raw),
                None => Ok(Vec::new()),
            }
        }
    }
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
    match serde_json::from_str::<CalendarFile>(&raw) {
        Ok(file) => Ok(file.items),
        Err(error) => Err(backup_corrupt(ctx, raw, &error).await),
    }
}

/// 原文另存为同目录的 `.corrupt-*` 备份后删除本体；备份失败即中止（不丢原文）。
async fn backup_corrupt(ctx: &Context, raw: String, error: &serde_json::Error) -> String {
    let backup = format!(".atelyx/calendar.json.corrupt-{:08x}", random_tag());
    if let Err(write) = ctx.vault_write_text(&backup, &raw).await {
        return format!("日程文件损坏且原文备份失败（{backup}），已中止读取以免覆盖原文：{write}");
    }
    let name = backup.rsplit('/').next().unwrap_or(backup.as_str()).to_string();
    match ctx.vault_remove(CALENDAR_FILE).await {
        Ok(()) => format!("日程文件损坏，原文已备份为 {name}，修复或删除后重试：{error}"),
        Err(remove) => format!("日程文件损坏，原文已备份为 {name}，但原文件删除失败：{remove}"),
    }
}

fn random_tag() -> u32 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_i64(date::now_ms());
    hasher.finish() as u32
}

fn calendar_json(items: &[CalendarItem]) -> Result<String, String> {
    serde_json::to_string(&CalendarFile { schema: SCHEMA.into(), items: items.to_vec() })
        .map_err(|e| e.to_string())
}

/// 读侧口径：空原文 = 空清单，损坏 = Err。
fn parse_strict(raw: &str) -> Result<Vec<CalendarItem>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<CalendarFile>(raw)
        .map(|file| file.items)
        .map_err(|e| format!("日程数据损坏：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trips_with_schema_wrapper() {
        let items = vec![CalendarItem {
            id: "a".into(),
            date: "2026-10-01".into(),
            title: "写方案".into(),
            note: Some("备注".into()),
            color: Some("#e06c75".into()),
            created_at: 7,
        }];
        let raw = calendar_json(&items).unwrap();
        assert!(raw.contains(SCHEMA));
        assert_eq!(parse_strict(&raw).unwrap(), items);
    }

    #[test]
    fn parse_treats_empty_as_empty_but_corrupt_as_error() {
        assert!(parse_strict("").unwrap().is_empty());
        assert!(parse_strict("   ").unwrap().is_empty());
        assert!(parse_strict("{not json").is_err());
    }

    #[test]
    fn missing_item_fields_fall_back_to_defaults() {
        let items = parse_strict(r#"{"schema":"atelyx-calendar/v1","items":[{},{"id":"x","date":"2026-02-01","title":"t"}]}"#)
            .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "");
        assert_eq!(items[1].color, None);
        assert_eq!(items[1].created_at, 0);
    }

    #[test]
    fn corrupt_backup_tags_differ() {
        assert_ne!(random_tag(), random_tag());
    }
}
