//! 日历插件状态：当前仓库日程、选中日期、月份视图与添加草稿（纯操作，无 I/O）。

use std::collections::HashMap;

use atelyx_plugin_protocol::method::{CalendarAddParams, CalendarItem};

use crate::date;

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
pub const PANEL: &str = "calendar";

/// 主题取到之前的缺省小号字号（网格日期与日程标题用）。
pub const DEFAULT_MICRO: f32 = 11.0;

/// 界面产出的用户动作；宿主调用由入口层执行，状态只在这里改。
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Add,
    Delete(String),
}

/// 一条日程的渲染快照。
#[derive(Clone, Debug, PartialEq)]
pub struct EventView {
    pub id: String,
    pub title: String,
    pub note: Option<String>,
    pub color: Option<String>,
}

impl From<&CalendarItem> for EventView {
    fn from(item: &CalendarItem) -> Self {
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            note: item.note.clone().filter(|note| !note.trim().is_empty()),
            color: item.color.clone(),
        }
    }
}

/// 一帧渲染所需的只读数据：出帧期间不再回读可变状态。
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub year: i64,
    pub month: u32,
    pub cells: Vec<Option<u32>>,
    pub selected: i64,
    pub today: i64,
    pub heading: String,
    pub events: Vec<EventView>,
    /// 按自纪元天数归组的标记（月网格每格最多画两条标题）。
    pub by_day: HashMap<i64, Vec<EventView>>,
    pub draft: String,
    pub error: Option<String>,
    pub pending_delete: Option<String>,
    pub ready: bool,
    pub micro: f32,
}

impl Snapshot {
    pub fn month_title(&self) -> String {
        date::month_title(self.year, self.month)
    }
}

/// 插件自持状态；日程本身永远以宿主回读的清单为准。
pub struct Model {
    items: Vec<CalendarItem>,
    selected: i64,
    today: i64,
    view: (i64, u32),
    draft: String,
    error: Option<String>,
    pending_delete: Option<String>,
    ready: bool,
    micro: f32,
}

impl Model {
    pub fn new(micro: f32) -> Self {
        let today = date::today();
        let (year, month, _) = date::civil_from_days(today);
        Self {
            items: Vec::new(),
            selected: today,
            today,
            view: (year, month),
            draft: String::new(),
            error: None,
            pending_delete: None,
            ready: false,
            micro,
        }
    }

    /// 月网格日期文字的字号随宿主主题。
    pub fn set_micro(&mut self, micro: f32) {
        self.micro = micro;
    }

    /// 用宿主回读的清单整体替换（按日期、创建时间定序）。
    pub fn replace(&mut self, mut items: Vec<CalendarItem>) {
        items.sort_by(|a, b| a.date.cmp(&b.date).then(a.created_at.cmp(&b.created_at)));
        self.items = items;
        self.today = date::today();
        self.ready = true;
        self.error = None;
        if let Some(pending) = &self.pending_delete {
            if !self.items.iter().any(|item| &item.id == pending) {
                self.pending_delete = None;
            }
        }
    }

    /// 宿主调用失败：留有可读原因，界面照样出帧（数据仍是上次回读的清单）。
    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.ready = true;
    }

    pub fn select(&mut self, day: i64) {
        self.selected = day;
        let (year, month, _) = date::civil_from_days(day);
        self.view = (year, month);
        self.pending_delete = None;
        self.error = None;
    }

    /// 翻月只动视图月份，选中日不变（翻回该月仍停在原来的选中日）。
    pub fn shift_month(&mut self, delta: i32) {
        let (year, month) = self.view;
        self.view = date::month_add(year, month, delta);
        self.error = None;
    }

    pub fn show_today(&mut self) {
        self.select(date::today());
    }

    pub fn set_draft(&mut self, value: String) {
        self.draft = value;
        self.error = None;
    }

    pub fn clear_draft(&mut self) {
        self.draft.clear();
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// 草稿 → 新增入参：空白草稿不发请求，回可读提示。
    pub fn add_params(&self) -> Result<CalendarAddParams, String> {
        let title = self.draft.trim();
        if title.is_empty() {
            return Err("先写一条日程".into());
        }
        Ok(CalendarAddParams {
            date: date::date_string(self.selected),
            title: title.to_string(),
            note: None,
            color: None,
        })
    }

    pub fn request_delete(&mut self, id: &str) {
        self.pending_delete = Some(id.to_string());
        self.error = None;
    }

    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    pub fn snapshot(&self) -> Snapshot {
        let (year, month) = self.view;
        let events: Vec<EventView> = self
            .items
            .iter()
            .filter(|item| date::parse_date(&item.date) == Some(self.selected))
            .map(EventView::from)
            .collect();
        let mut by_day: HashMap<i64, Vec<EventView>> = HashMap::new();
        for item in &self.items {
            if let Some(day) = date::parse_date(&item.date) {
                by_day.entry(day).or_default().push(EventView::from(item));
            }
        }
        Snapshot {
            year,
            month,
            cells: date::month_cells(year, month),
            selected: self.selected,
            today: self.today,
            heading: date::day_title(self.selected),
            events,
            by_day,
            draft: self.draft.clone(),
            error: self.error.clone(),
            pending_delete: self.pending_delete.clone(),
            ready: self.ready,
            micro: self.micro,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn replace_sorts_by_date_then_creation() {
        let mut model = Model::new(11.0);
        model.replace(vec![item("b", "2026-10-02", 2), item("a", "2026-10-01", 9), item("c", "2026-10-01", 1)]);
        let ids: Vec<&str> = model.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b"]);
        assert!(model.ready);
    }

    #[test]
    fn snapshot_groups_marks_by_day_and_lists_the_selection() {
        let mut model = Model::new(11.0);
        model.replace(vec![item("a", "2026-10-01", 1), item("b", "2026-10-01", 2), item("c", "2026-10-05", 3), item("bad", "2026-13-40", 4)]);
        model.select(date::days_from_civil(2026, 10, 1));
        let snapshot = model.snapshot();
        assert_eq!(snapshot.events.len(), 2);
        assert_eq!(snapshot.by_day.get(&date::days_from_civil(2026, 10, 1)).map(Vec::len), Some(2));
        assert_eq!(snapshot.by_day.len(), 2, "非法日期不进网格");
        assert!(snapshot.cells.contains(&Some(1)));
        assert_eq!(snapshot.month_title(), "2026 年 10 月");
    }

    #[test]
    fn add_params_require_a_non_blank_title_and_use_the_selected_day() {
        let mut model = Model::new(11.0);
        model.select(date::days_from_civil(2026, 10, 9));
        assert!(model.add_params().is_err());
        model.set_draft("  写方案  ".into());
        let params = model.add_params().unwrap();
        assert_eq!(params.date, "2026-10-09");
        assert_eq!(params.title, "写方案");
    }

    #[test]
    fn shift_month_keeps_the_selection() {
        let mut model = Model::new(11.0);
        model.select(date::days_from_civil(2026, 10, 9));
        model.shift_month(1);
        let snapshot = model.snapshot();
        assert_eq!((snapshot.year, snapshot.month), (2026, 11));
        assert_eq!(snapshot.selected, date::days_from_civil(2026, 10, 9));
    }

    #[test]
    fn a_vanished_pending_delete_is_cleared_by_the_next_read() {
        let mut model = Model::new(11.0);
        model.replace(vec![item("a", "2026-10-01", 1)]);
        model.request_delete("a");
        model.replace(Vec::new());
        assert_eq!(model.pending_delete, None);
    }
}
