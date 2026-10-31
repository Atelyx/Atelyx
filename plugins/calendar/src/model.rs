//! 日历插件状态：容器去向、日程清单、月份视图、编辑会话与仓库历史（纯操作，无 I/O）。

use std::collections::HashMap;

use atelyx_plugin_protocol::method::{
    CalendarAddParams, CalendarItem, HistoryDayEntry, ThemeResult,
};

use crate::date;
use crate::store::Backend;

/// 面板贡献 id，同时是该面挂载后的 surfaceId。
pub const PANEL: &str = "calendar";

/// 主题取到之前的缺省字号与圆角（与宿主缺省档一致）。
const DEFAULT_MICRO: f32 = 11.0;
const DEFAULT_CAPTION: f32 = 12.0;
const DEFAULT_BODY: f32 = 14.0;
const DEFAULT_RADIUS_XS: f32 = 4.0;
const DEFAULT_RADIUS_SM: f32 = 6.0;

/// 绘制用主题快照：字号 / 圆角 / 强调色 / 事件色板。
#[derive(Clone, Debug)]
pub struct Palette {
    pub micro: f32,
    pub caption: f32,
    pub body: f32,
    pub radius_xs: f32,
    pub radius_sm: f32,
    pub accent: String,
    pub event_colors: Vec<String>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            micro: DEFAULT_MICRO,
            caption: DEFAULT_CAPTION,
            body: DEFAULT_BODY,
            radius_xs: DEFAULT_RADIUS_XS,
            radius_sm: DEFAULT_RADIUS_SM,
            accent: String::new(),
            event_colors: Vec::new(),
        }
    }
}

impl Palette {
    pub fn from_theme(theme: &ThemeResult) -> Self {
        Self {
            micro: theme.font_sizes.micro,
            caption: theme.font_sizes.caption,
            body: theme.font_sizes.body,
            radius_xs: theme.radii.xs,
            radius_sm: theme.radii.sm,
            accent: theme.colors.accent.clone(),
            event_colors: theme.event_colors.clone(),
        }
    }
}

/// 一帧渲染所需的只读数据：出帧期间不再回读可变状态。
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// 是否打开了仓库 / 空间（否 = 整面给未打开提示）。
    pub opened: bool,
    pub writable: bool,
    pub blocked: Option<&'static str>,
    pub ready: bool,
    pub year: i64,
    pub month: u32,
    pub cells: Vec<Option<u32>>,
    pub today: i64,
    pub selected: Option<i64>,
    pub by_day: HashMap<i64, Vec<CalendarItem>>,
    pub events: Vec<CalendarItem>,
    pub counts: HashMap<i64, u32>,
    pub activity_line: Option<String>,
    pub draft: String,
    pub draft_color: usize,
    pub edit: Option<(String, String)>,
    pub delete_confirm: Option<String>,
    pub error: Option<String>,
    pub history: Option<Result<Vec<HistoryDayEntry>, String>>,
    pub palette: Palette,
}

impl Snapshot {
    pub fn month_title(&self) -> String {
        format!("{}年{}月", self.year, self.month)
    }

    pub fn day_title(&self) -> Option<String> {
        self.selected.map(date::day_label)
    }
}

/// 插件自持状态；日程真源永远是当前容器的存储（本地文件 / 空间 meta）。
pub struct Model {
    backend: Option<Backend>,
    items: Vec<CalendarItem>,
    ready: bool,
    month_offset: i32,
    selected: Option<i64>,
    draft: String,
    draft_color: usize,
    edit: Option<(String, String)>,
    focus_edit: bool,
    delete_confirm: Option<String>,
    error: Option<String>,
    activity: Option<Result<HashMap<i64, u32>, String>>,
    activity_loading: bool,
    history: Option<Result<Vec<HistoryDayEntry>, String>>,
    palette: Palette,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Self {
        Self {
            backend: None,
            items: Vec::new(),
            ready: false,
            month_offset: 0,
            selected: None,
            draft: String::new(),
            draft_color: 0,
            edit: None,
            focus_edit: false,
            delete_confirm: None,
            error: None,
            activity: None,
            activity_loading: false,
            history: None,
            palette: Palette::default(),
        }
    }

    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn backend(&self) -> Option<Backend> {
        self.backend
    }

    /// 容器切换（含首次订阅回放）：旧容器的日程、选中日与在途状态全部作废。
    pub fn set_backend(&mut self, backend: Backend) {
        self.backend = Some(backend);
        self.items.clear();
        self.ready = false;
        self.month_offset = 0;
        self.selected = None;
        self.draft.clear();
        self.draft_color = 0;
        self.edit = None;
        self.focus_edit = false;
        self.delete_confirm = None;
        self.error = None;
        self.activity = None;
        self.activity_loading = false;
        self.history = None;
    }

    /// 写操作的拦截原因（只读身份 / 仓库未打开）；读操作不受限。
    pub fn blocked_reason(&self) -> Option<&'static str> {
        match self.backend {
            None => Some("仓库未打开"),
            Some(Backend::Space { can_write: false }) => Some("只读身份无法修改日程"),
            Some(_) => None,
        }
    }

    pub fn writable(&self) -> bool {
        self.blocked_reason().is_none()
    }

    /// 用真源清单替换（展示层按日期 / 创建序归组，这里保持读回顺序）。
    pub fn replace(&mut self, items: Vec<CalendarItem>) {
        self.items = items;
        self.ready = true;
        if let Some(pending) = &self.delete_confirm {
            if !self.items.iter().any(|item| &item.id == pending) {
                self.delete_confirm = None;
            }
        }
    }

    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.ready = true;
    }

    pub fn set_activity(&mut self, counts: Result<HashMap<i64, u32>, String>) {
        self.activity_loading = false;
        self.activity = Some(counts);
    }

    pub fn set_activity_loading(&mut self) {
        self.activity_loading = true;
    }

    pub fn set_history(&mut self, day: i64, entries: Result<Vec<HistoryDayEntry>, String>) {
        if self.selected != Some(day) {
            return;
        }
        self.history = Some(entries);
    }

    /// 当日历史条目（与快照列表同序，供点击行取回打开参数）。
    pub fn history_entry(&self, index: usize) -> Option<HistoryDayEntry> {
        match &self.history {
            Some(Ok(entries)) => entries.get(index).cloned(),
            _ => None,
        }
    }

    pub fn selected(&self) -> Option<i64> {
        self.selected
    }

    /// 选中日的历史还没拉到（选中或切仓后）时，返回该日供调用方补拉。
    pub fn pending_history_day(&self) -> Option<i64> {
        self.selected.filter(|_| self.history.is_none())
    }

    pub fn view_month(&self) -> (i64, u32) {
        let (year, month, _) = date::civil_from_days(date::today());
        date::month_add(year, month, self.month_offset)
    }

    pub fn shift_month(&mut self, delta: i32) {
        self.month_offset += delta;
        self.delete_confirm = None;
        self.error = None;
    }

    pub fn show_today(&mut self) {
        self.month_offset = 0;
        self.select(date::today());
    }

    /// 选中某天：详情与历史随之切换，删除确认与错误提示作废。
    pub fn select(&mut self, day: i64) {
        self.selected = Some(day);
        self.delete_confirm = None;
        self.error = None;
        self.history = None;
    }

    pub fn set_draft(&mut self, value: String) {
        self.draft = value;
        self.error = None;
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn clear_draft(&mut self) {
        self.draft.clear();
    }

    pub fn set_draft_color(&mut self, index: usize) {
        self.draft_color = index;
        self.error = None;
    }

    fn draft_color_value(&self) -> Option<String> {
        let colors = &self.palette.event_colors;
        if colors.is_empty() {
            return None;
        }
        colors.get(self.draft_color % colors.len()).cloned()
    }

    /// 草稿 → 新增入参：未选日期 / 空白草稿 / 超长标题不发请求，回可读提示。
    pub fn add_params(&self) -> Result<CalendarAddParams, String> {
        let Some(day) = self.selected else {
            return Err("先点选一个日期".into());
        };
        let title = self.draft.trim();
        if title.is_empty() {
            return Err("先写一条日程".into());
        }
        let params = CalendarAddParams {
            date: date::date_string(day),
            title: title.to_string(),
            note: None,
            color: self.draft_color_value(),
        };
        params.validate()?;
        Ok(params)
    }

    /// 进入标题行内编辑，并请求下一帧聚焦该输入框。
    pub fn begin_edit(&mut self, id: &str) {
        let Some(item) = self.items.iter().find(|item| item.id == id) else { return };
        self.edit = Some((id.to_string(), item.title.clone()));
        self.focus_edit = true;
        self.delete_confirm = None;
        self.error = None;
    }

    pub fn editing(&self, id: &str) -> bool {
        self.edit.as_ref().is_some_and(|(editing, _)| editing == id)
    }

    /// 正在编辑的条目 id（行内输入框的标识由它派生）。
    pub fn edit_target(&self) -> Option<&str> {
        self.edit.as_ref().map(|(id, _)| id.as_str())
    }

    pub fn edit_draft(&self) -> Option<String> {
        self.edit.as_ref().map(|(_, draft)| draft.clone())
    }

    pub fn set_edit_draft(&mut self, value: String) {
        if let Some((_, draft)) = &mut self.edit {
            *draft = value;
        }
    }

    /// 收走编辑会话（提交 / 取消都经这里，后续失焦不再重复提交）。
    pub fn take_edit(&mut self) -> Option<(String, String)> {
        self.edit.take()
    }

    pub fn cancel_edit(&mut self) {
        self.edit = None;
        self.focus_edit = false;
    }

    /// 下一帧应把焦点放到编辑输入框时的目标与初始文本。
    pub fn take_focus_edit(&mut self) -> Option<(String, String)> {
        if !self.focus_edit {
            return None;
        }
        self.focus_edit = false;
        self.edit.clone()
    }

    pub fn request_delete(&mut self, id: &str) {
        self.delete_confirm = Some(id.to_string());
        self.error = None;
    }

    pub fn cancel_delete(&mut self) {
        self.delete_confirm = None;
    }

    pub fn snapshot(&self) -> Snapshot {
        let (year, month) = self.view_month();
        let today = date::today();
        let mut by_day: HashMap<i64, Vec<CalendarItem>> = HashMap::new();
        for item in &self.items {
            if let Some(day) = date::parse_date(&item.date) {
                by_day.entry(day).or_default().push(item.clone());
            }
        }
        for events in by_day.values_mut() {
            events.sort_by_key(|item| item.created_at);
        }
        let events = self
            .selected
            .and_then(|day| by_day.get(&day).cloned())
            .unwrap_or_default();
        let counts = match &self.activity {
            Some(Ok(counts)) => counts.clone(),
            _ => HashMap::new(),
        };
        let activity_line = match &self.activity {
            Some(Err(error)) => Some(format!("活动统计失败：{error}")),
            None if self.activity_loading => Some("活动统计中…".into()),
            _ => None,
        };
        Snapshot {
            opened: self.backend.is_some(),
            writable: self.writable(),
            blocked: self.blocked_reason(),
            ready: self.ready,
            year,
            month,
            cells: date::month_cells(year, month),
            today,
            selected: self.selected,
            by_day,
            events,
            counts,
            activity_line,
            draft: self.draft.clone(),
            draft_color: self.draft_color,
            edit: self.edit.clone(),
            delete_confirm: self.delete_confirm.clone(),
            error: self.error.clone(),
            history: self.history.clone(),
            palette: self.palette.clone(),
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

    fn with_palette() -> Model {
        let mut model = Model::new();
        model.set_palette(Palette {
            event_colors: vec!["#111111".into(), "#222222".into()],
            ..Palette::default()
        });
        model
    }

    #[test]
    fn snapshot_groups_by_day_and_lists_the_selection_in_creation_order() {
        let mut model = Model::new();
        model.set_backend(Backend::Local);
        model.replace(vec![
            item("b", "2026-10-01", 2),
            item("a", "2026-10-01", 1),
            item("c", "2026-10-05", 3),
            item("bad", "2026-13-40", 4),
        ]);
        model.select(date::days_from_civil(2026, 10, 1));
        let snapshot = model.snapshot();
        let ids: Vec<&str> = snapshot.events.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(snapshot.by_day.len(), 2, "非法日期不进网格");
        assert_eq!(snapshot.by_day.get(&date::days_from_civil(2026, 10, 1)).map(Vec::len), Some(2));
        assert!(snapshot.cells.contains(&Some(1)));
    }

    #[test]
    fn shift_month_keeps_the_selection_and_today_resets_it() {
        let mut model = Model::new();
        model.set_backend(Backend::Local);
        model.select(date::days_from_civil(2026, 10, 9));
        model.shift_month(1);
        let snapshot = model.snapshot();
        assert_eq!((snapshot.year, snapshot.month), (2026, 11));
        assert_eq!(snapshot.selected, Some(date::days_from_civil(2026, 10, 9)));
        model.show_today();
        assert_eq!(model.snapshot().month_title(), {
            let (y, m, _) = date::civil_from_days(date::today());
            format!("{y}年{m}月")
        });
    }

    #[test]
    fn add_params_require_a_selection_and_a_non_blank_title() {
        let mut model = with_palette();
        model.set_backend(Backend::Local);
        assert!(model.add_params().is_err(), "未选日期");
        model.select(date::days_from_civil(2026, 10, 9));
        assert!(model.add_params().is_err(), "空草稿");
        model.set_draft("  写方案  ".into());
        model.set_draft_color(1);
        let params = model.add_params().unwrap();
        assert_eq!(params.date, "2026-10-09");
        assert_eq!(params.title, "写方案");
        assert_eq!(params.color.as_deref(), Some("#222222"));
        model.set_draft("x".repeat(201));
        assert!(model.add_params().is_err(), "超长标题在入参处拦下");
    }

    #[test]
    fn write_permission_follows_the_container() {
        let mut model = Model::new();
        assert_eq!(model.blocked_reason(), Some("仓库未打开"));
        model.set_backend(Backend::Local);
        assert!(model.writable());
        model.set_backend(Backend::Space { can_write: false });
        assert_eq!(model.blocked_reason(), Some("只读身份无法修改日程"));
        model.set_backend(Backend::Space { can_write: true });
        assert!(model.writable());
    }

    #[test]
    fn switching_containers_drops_the_old_session() {
        let mut model = with_palette();
        model.set_backend(Backend::Local);
        model.replace(vec![item("a", "2026-10-01", 1)]);
        model.select(date::days_from_civil(2026, 10, 1));
        model.set_draft("草稿".into());
        model.request_delete("a");
        model.set_backend(Backend::Space { can_write: true });
        let snapshot = model.snapshot();
        assert!(snapshot.events.is_empty());
        assert_eq!(snapshot.selected, None);
        assert!(snapshot.draft.is_empty());
        assert_eq!(snapshot.delete_confirm, None);
        assert!(!snapshot.ready);
    }

    #[test]
    fn a_vanished_pending_delete_is_cleared_by_the_next_read() {
        let mut model = Model::new();
        model.set_backend(Backend::Local);
        model.replace(vec![item("a", "2026-10-01", 1)]);
        model.request_delete("a");
        model.replace(Vec::new());
        assert_eq!(model.snapshot().delete_confirm, None);
    }

    #[test]
    fn edit_session_reports_the_initial_draft_once() {
        let mut model = with_palette();
        model.set_backend(Backend::Local);
        model.replace(vec![item("a", "2026-10-01", 1)]);
        model.begin_edit("a");
        assert_eq!(model.take_focus_edit(), Some(("a".into(), "日程 a".into())));
        assert_eq!(model.take_focus_edit(), None);
        assert_eq!(model.edit_target(), Some("a"));
        model.set_edit_draft("改".into());
        assert_eq!(model.take_edit(), Some(("a".into(), "改".into())));
        assert_eq!(model.edit_target(), None);
        assert_eq!(model.take_edit(), None);
    }
}
