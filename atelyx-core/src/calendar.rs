//! 日历手动日程存储：`.atelyx/calendar.json`（schema `atelyx-calendar/v1`）读写与条目纯操作。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 日程存储文件 schema 版本（读侧不校验，仅写盘落字段）。
pub const CALENDAR_SCHEMA: &str = "atelyx-calendar/v1";

/// 手动日程存储文件（仓库级相对路径）。
pub const CALENDAR_FILE: &str = ".atelyx/calendar.json";

/// 手动日程条目（date 为本地日期 `YYYY-MM-DD`；color 为 hex，缺省 = 默认强调色）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct CalendarFile {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    items: Vec<CalendarItem>,
}

fn calendar_path(root: &Path) -> PathBuf {
    root.join(CALENDAR_FILE)
}

/// 读取手动日程：缺失 → 空列表；其余失败一律 Err，不降级为空清单。
///
/// 为什么不按空继续：本地写路径是「读 → 改 → 整文件覆盖」，读失败按空继续会在用户
/// 下一次添加日程时把既有日程整文件清空。损坏时原文经 `backup_corrupt_config` 改名
/// 留档后仍返回 Err（数据在备份里，本体待用户处理后重写）。
pub fn read_calendar(root: &Path) -> Result<Vec<CalendarItem>, String> {
    let path = calendar_path(root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读取日程文件失败（{}）：{e}", path.display())),
    };
    match serde_json::from_str::<CalendarFile>(&raw) {
        Ok(file) => Ok(file.items),
        Err(e) => {
            let Some(backup) = crate::vault::backup_corrupt_config(&path, "calendar") else {
                return Err(format!(
                    "日程文件损坏且原文备份失败（{}），已中止读取以免覆盖原文：{e}",
                    path.display()
                ));
            };
            eprintln!("[calendar] 日程文件损坏，已备份为 {}：{e}", backup.display());
            Err(format!(
                "日程文件损坏，原文已备份为 {}，修复或删除后重试",
                backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ))
        }
    }
}

/// 条目清单 → 日程 JSON 原文（schema 包装；写盘与空间 meta 值共用同一编码）。
pub fn calendar_json(items: &[CalendarItem]) -> Result<String, String> {
    serde_json::to_string(&CalendarFile {
        schema: CALENDAR_SCHEMA.into(),
        items: items.to_vec(),
    })
    .map_err(|e| e.to_string())
}

/// 日程 JSON 原文解析（展示用宽松口径：空值/损坏按空清单，与 meta 键缺失同视）。
pub fn parse_calendar_json(raw: &str) -> Vec<CalendarItem> {
    serde_json::from_str::<CalendarFile>(raw)
        .map(|f| f.items)
        .unwrap_or_default()
}

/// 日程 JSON 原文严格解析（写前校验：非空原文损坏 = Err，防读改写链清空服务端数据）。
pub fn parse_calendar_json_strict(raw: &str) -> Result<Vec<CalendarItem>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<CalendarFile>(raw)
        .map(|f| f.items)
        .map_err(|e| format!("日程数据损坏：{e}"))
}

/// 全量写入手动日程（经 `vault::atomic_write` 原子落盘）。
pub fn write_calendar(root: &Path, items: &[CalendarItem]) -> Result<(), String> {
    crate::vault::atomic_write(&calendar_path(root), &calendar_json(items)?)
}

/// 按 id 替换（保持原位）或追加条目。
pub fn upsert(items: &[CalendarItem], item: CalendarItem) -> Vec<CalendarItem> {
    let mut next = items.to_vec();
    if let Some(slot) = next.iter_mut().find(|it| it.id == item.id) {
        *slot = item;
    } else {
        next.push(item);
    }
    next
}

/// 按 id 移除条目。
pub fn remove(items: &[CalendarItem], id: &str) -> Vec<CalendarItem> {
    items.iter().filter(|it| it.id != id).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    fn item(id: &str, date: &str, created_at: i64) -> CalendarItem {
        CalendarItem {
            id: id.into(),
            date: date.into(),
            title: format!("日程 {id}"),
            note: None,
            color: None,
            created_at,
        }
    }

    #[test]
    fn missing_file_reads_empty() {
        let tmp = TempDir::new("cal-missing");
        assert!(read_calendar(&tmp).unwrap().is_empty());
    }

    #[test]
    fn round_trip_preserves_items() {
        let tmp = TempDir::new("cal-roundtrip");
        let items = vec![
            CalendarItem {
                note: Some("备注".into()),
                color: Some("#e06c75".into()),
                ..item("a", "2026-01-02", 10)
            },
            item("b", "2026-01-01", 5),
        ];
        write_calendar(&tmp, &items).unwrap();
        assert_eq!(read_calendar(&tmp).unwrap(), items);
    }

    #[test]
    fn missing_fields_default_on_read() {
        let tmp = TempDir::new("cal-defaults");
        std::fs::create_dir_all(tmp.join(".atelyx")).unwrap();
        std::fs::write(
            calendar_path(&tmp),
            r#"{"schema":"atelyx-calendar/v1","items":[{},{"id":"x","date":"2026-02-01","title":"t"}]}"#,
        )
        .unwrap();
        let items = read_calendar(&tmp).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0],
            CalendarItem { id: String::new(), date: String::new(), title: String::new(), note: None, color: None, created_at: 0 }
        );
        assert_eq!(items[1].note, None);
        assert_eq!(items[1].color, None);
        assert_eq!(items[1].created_at, 0);
    }

    #[test]
    fn corrupt_file_backed_up_and_reads_err() {
        let tmp = TempDir::new("cal-corrupt");
        let path = calendar_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        let err = read_calendar(&tmp).unwrap_err();
        assert!(err.contains("备份"));
        // 原文已改名留档：本体不在，目录下出现 corrupt 备份
        assert!(!path.exists());
        let backed_up = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with("calendar.json.corrupt-"));
        assert!(backed_up);
    }

    #[test]
    fn strict_parse_rejects_corrupt_but_accepts_empty() {
        assert!(parse_calendar_json_strict("   ").unwrap().is_empty());
        assert_eq!(parse_calendar_json_strict(r#"{"schema":"atelyx-calendar/v1","items":[]}"#).unwrap().len(), 0);
        assert!(parse_calendar_json_strict("{bad").is_err());
    }

    #[test]
    fn upsert_replaces_in_place_or_appends() {
        let items = vec![item("a", "2026-01-01", 1), item("b", "2026-01-02", 2)];
        let replaced = upsert(&items, item("b", "2026-01-03", 3));
        assert_eq!(replaced.len(), 2);
        assert_eq!(replaced[1].date, "2026-01-03");
        let appended = upsert(&items, item("c", "2026-01-04", 4));
        assert_eq!(appended.len(), 3);
        assert_eq!(appended[2].id, "c");
    }

    #[test]
    fn remove_filters_by_id() {
        let items = vec![item("a", "2026-01-01", 1), item("b", "2026-01-02", 2)];
        let removed = remove(&items, "a");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id, "b");
        // 不存在的 id：原样返回
        assert_eq!(remove(&items, "zzz"), items);
    }
}
