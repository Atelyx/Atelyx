//! 日程条目纯操作：服务调用与界面变更在同一份清单上应用，返回给调用方的回执。
//! 排序、按色板循环与条目 id 生成都在这里，入口层只负责读写真源。

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use serde_json::Value;

use atelyx_plugin_protocol::method::{
    CalendarAddParams, CalendarItem, CalendarListResult, CalendarUpdateParams,
};

/// 一次写入：界面与服务调用共用同一应用口径。
#[derive(Clone, Debug, PartialEq)]
pub enum Mutation {
    Add(CalendarAddParams),
    Update(CalendarUpdateParams),
    SetTitle(String, String),
    CycleColor(String),
    Remove(String),
}

/// 在清单上应用一次变更；`None` = 没有实际改动（不落盘）。
/// `palette` 为空时换色不生效，`Add` 的颜色按调用方给出。
pub fn apply(
    items: &mut Vec<CalendarItem>,
    mutation: &Mutation,
    seq: u64,
    now_ms: i64,
    palette: &[String],
) -> Result<Option<Value>, String> {
    match mutation {
        Mutation::Add(params) => {
            let item = CalendarItem {
                id: next_item_id(now_ms, seq),
                date: params.date.clone(),
                title: params.title.trim().to_string(),
                note: params.note.clone(),
                color: params.color.clone(),
                created_at: now_ms,
            };
            *items = upsert(items, item.clone());
            serde_json::to_value(&item)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        Mutation::Update(params) => {
            let slot = find(items, &params.id)?;
            if let Some(date) = &params.date {
                slot.date = date.clone();
            }
            if let Some(title) = &params.title {
                slot.title = title.trim().to_string();
            }
            // 空串 = 清空备注（与「未给出 = 保持原值」区分）
            if let Some(note) = &params.note {
                slot.note = (!note.is_empty()).then(|| note.clone());
            }
            if let Some(color) = &params.color {
                slot.color = Some(color.clone());
            }
            Ok(Some(serde_json::json!({})))
        }
        Mutation::SetTitle(id, title) => {
            let title = title.trim();
            if title.is_empty() {
                return Err("标题不能为空".into());
            }
            let slot = find(items, id)?;
            if slot.title == title {
                return Ok(None);
            }
            slot.title = title.to_string();
            Ok(Some(serde_json::json!({})))
        }
        Mutation::CycleColor(id) => {
            let slot = find(items, id)?;
            let Some(next) = next_color(slot.color.as_deref(), palette) else {
                return Ok(None);
            };
            if slot.color.as_deref() == Some(next.as_str()) {
                return Ok(None);
            }
            slot.color = Some(next);
            Ok(Some(serde_json::json!({})))
        }
        Mutation::Remove(id) => {
            find(items, id)?;
            items.retain(|item| item.id != *id);
            Ok(Some(serde_json::json!({})))
        }
    }
}

fn find<'a>(items: &'a mut [CalendarItem], id: &str) -> Result<&'a mut CalendarItem, String> {
    items
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("日程不存在：{id}"))
}

fn upsert(items: &[CalendarItem], item: CalendarItem) -> Vec<CalendarItem> {
    let mut next = items.to_vec();
    if let Some(slot) = next.iter_mut().find(|it| it.id == item.id) {
        *slot = item;
    } else {
        next.push(item);
    }
    next
}

/// `calendar.list` 的回执：按日期、创建时间与 id 稳定排序。
pub fn list_result(items: &[CalendarItem]) -> CalendarListResult {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.created_at.cmp(&b.created_at))
            .then(a.id.cmp(&b.id))
    });
    CalendarListResult { items: sorted }
}

/// 色板循环：当前色在色板内取下一色，不在色板内从首色开始；色板为空 = 不换色。
pub fn next_color(current: Option<&str>, palette: &[String]) -> Option<String> {
    if palette.is_empty() {
        return None;
    }
    let next = current
        .and_then(|hex| palette.iter().position(|color| color.eq_ignore_ascii_case(hex)))
        .map_or(0, |index| (index + 1) % palette.len());
    Some(palette[next].clone())
}

/// 新建条目 id：毫秒时间戳 + 会话序号 + 随机 hex 后缀，异端同毫秒同序号也不互替。
pub fn next_item_id(now_ms: i64, seq: u64) -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(now_ms as u64);
    hasher.write_u64(seq);
    format!("ev-{now_ms}-{seq}-{:06x}", hasher.finish() as u32 & 0xffffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str) -> CalendarItem {
        CalendarItem {
            id: id.into(),
            date: "2026-10-13".into(),
            title: title.into(),
            note: None,
            color: None,
            created_at: 1,
        }
    }

    fn add_params(title: &str) -> CalendarAddParams {
        CalendarAddParams { date: "2026-10-30".into(), title: title.into(), note: None, color: None }
    }

    #[test]
    fn add_upserts_with_trimmed_title_and_generated_id() {
        let mut items = Vec::new();
        let value = apply(&mut items, &Mutation::Add(add_params("  会议  ")), 1, 42, &[])
            .unwrap()
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "会议");
        assert_eq!(items[0].created_at, 42);
        assert_eq!(value["id"], items[0].id.as_str());
        assert_ne!(next_item_id(42, 1), next_item_id(42, 1), "同毫秒同序号仍应不同");
    }

    #[test]
    fn set_title_reports_no_change_for_the_same_text() {
        let mut items = vec![item("a", "旧")];
        assert!(apply(&mut items, &Mutation::SetTitle("a".into(), " 旧 ".into()), 0, 0, &[]).unwrap().is_none());
        assert!(apply(&mut items, &Mutation::SetTitle("a".into(), "改".into()), 0, 0, &[]).unwrap().is_some());
        assert_eq!(items[0].title, "改");
        assert!(apply(&mut items, &Mutation::SetTitle("zz".into(), "x".into()), 0, 0, &[]).is_err());
        assert!(apply(&mut items, &Mutation::SetTitle("a".into(), "  ".into()), 0, 0, &[]).is_err());
    }

    #[test]
    fn cycle_color_walks_the_palette_and_wraps() {
        let palette: Vec<String> = ["#111111", "#222222", "#333333"].map(String::from).to_vec();
        let mut items = vec![item("a", "x")];
        for expected in ["#111111", "#222222", "#333333", "#111111"] {
            assert!(apply(&mut items, &Mutation::CycleColor("a".into()), 0, 0, &palette)
                .unwrap()
                .is_some());
            assert_eq!(items[0].color.as_deref(), Some(expected));
        }
        // 大小写不敏感：色板外的颜色从首色开始
        let upper = vec!["#ABCDEF".to_string()];
        assert_eq!(next_color(Some("#abcdef"), &upper).as_deref(), Some("#ABCDEF"));
        assert_eq!(next_color(Some("#ffffff"), &upper).as_deref(), Some("#ABCDEF"));
        assert_eq!(next_color(Some("#ffffff"), &[]), None);
    }

    #[test]
    fn update_only_touches_given_fields_and_empty_note_clears() {
        let mut items = vec![CalendarItem {
            note: Some("备注".into()),
            color: Some("#112233".into()),
            ..item("a", "x")
        }];
        let update = CalendarUpdateParams {
            id: "a".into(),
            date: Some("2026-11-01".into()),
            title: Some("改".into()),
            note: Some(String::new()),
            color: None,
        };
        assert!(apply(&mut items, &Mutation::Update(update), 0, 0, &[]).unwrap().is_some());
        assert_eq!(items[0].date, "2026-11-01");
        assert_eq!(items[0].title, "改");
        assert_eq!(items[0].note, None);
        assert_eq!(items[0].color.as_deref(), Some("#112233"), "未给出的字段保持原值");
        let missing = CalendarUpdateParams { id: "zz".into(), date: None, title: Some("x".into()), note: None, color: None };
        assert!(apply(&mut items, &Mutation::Update(missing), 0, 0, &[]).unwrap_err().contains("不存在"));
    }

    #[test]
    fn remove_reports_missing_entries() {
        let mut items = vec![item("a", "x")];
        assert!(apply(&mut items, &Mutation::Remove("a".into()), 0, 0, &[]).unwrap().is_some());
        assert!(items.is_empty());
        assert!(apply(&mut items, &Mutation::Remove("a".into()), 0, 0, &[]).is_err());
    }

    #[test]
    fn list_result_sorts_by_date_then_creation() {
        let mut items = vec![
            CalendarItem { date: "2026-02-01".into(), created_at: 2, ..item("b", "x") },
            CalendarItem { date: "2026-01-01".into(), created_at: 9, ..item("c", "x") },
            CalendarItem { date: "2026-01-01".into(), created_at: 1, ..item("a", "x") },
        ];
        items.reverse();
        let sorted = list_result(&items);
        let ids: Vec<&str> = sorted.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "b"]);
    }
}
