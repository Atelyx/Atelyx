//! 日历日程数据链：手动日程（本地 `.atelyx/calendar.json` / 空间团队 meta `calendar` 键）的
//! 增删改与读写序、活动密度缓存装载；日期数学与视图渲染分别在 calendar_grid / calendar_view。

use std::collections::HashMap;

use iced::{Task, widget::operation::focus};

use atelyx_core::calendar::{self, CalendarItem};
use atelyx_core::history::day_key;
use atelyx_core::space_client::{SpaceClient, SpaceError};

use crate::calendar_grid;
use crate::repohistory_view;
use crate::space_actions;
use crate::ui::notify::Level as NoticeLevel;
use crate::ui::palette as pal;
use crate::{App, Message, RepoKind, layout_view};

/// 空间模式日程的团队 meta 键（服务端 meta 端点键名，值 = 与本地同构的日程 JSON）。
pub const META_KEY: &str = "calendar";

/// 标题行内输入的固定部件 id（同一时刻至多一个编辑会话）。
pub const EDIT_INPUT_ID: &str = "calendar-edit-input";

/// 热力图横向拖拽翻月的最小位移（逻辑像素）。
const HEAT_DRAG_THRESHOLD: f32 = 40.0;

/// 热力图拖拽会话：按压起点横坐标，松开时按位移方向翻月。
struct HeatDrag {
    window: iced::window::Id,
    start_x: f32,
}

/// 一次日程变更：内存按它即时推进，磁盘 / 服务端按它重放。
#[derive(Clone, Debug)]
pub enum CalendarOp {
    Add(CalendarItem),
    Remove(String),
    SetTitle(String, String),
    CycleColor(String),
}

/// 纯操作：按 id 应用变更（Add = upsert，其余按 id 定位）；返回是否实际改动。
pub fn apply_op(items: &mut Vec<CalendarItem>, op: &CalendarOp) -> bool {
    match op {
        CalendarOp::Add(item) => {
            *items = calendar::upsert(items, item.clone());
            true
        }
        CalendarOp::Remove(id) => {
            let changed = items.iter().any(|it| it.id == *id);
            *items = calendar::remove(items, id);
            changed
        }
        CalendarOp::SetTitle(id, title) => {
            let Some(slot) = items.iter_mut().find(|it| it.id == *id) else { return false };
            let changed = slot.title != *title;
            slot.title = title.clone();
            changed
        }
        CalendarOp::CycleColor(id) => {
            let Some(slot) = items.iter_mut().find(|it| it.id == *id) else { return false };
            let next = slot
                .color
                .as_deref()
                .and_then(color_index)
                .map_or(0, |i| (i + 1) % pal::EVENT_COLORS.len());
            slot.color = Some(color_hex(next));
            true
        }
    }
}

/// 色板下标 → hex（`CalendarItem.color` 存储格式）。
pub fn color_hex(idx: usize) -> String {
    let c = pal::EVENT_COLORS[idx % pal::EVENT_COLORS.len()];
    format!("#{:02x}{:02x}{:02x}", (c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8)
}

/// hex → 色板下标（不在色板内 = None，大小写不敏感）。
fn color_index(hex: &str) -> Option<usize> {
    let hex = hex.to_ascii_lowercase();
    (0..pal::EVENT_COLORS.len()).find(|i| color_hex(*i) == hex)
}

/// 日历视图会话态（容器切换整体重置）。
#[derive(Default)]
pub struct CalendarState {
    /// 月份偏移（0 = 当前月；‹ › 增减，热力图窗口随之平移）。
    pub month_offset: i32,
    /// 选中的天（自纪元天数；None = 未选，天详情区不展示）。
    pub selected: Option<i64>,
    /// 手动日程（当前容器；读回执与写回执收敛同一份）。
    pub items: Vec<CalendarItem>,
    /// 日程已装载（首次装载前展示加载态）。
    pub ready: bool,
    /// 装载在途。
    pub loading: bool,
    /// 用户变更序（读回执按它判定过期：变更后到达的旧读不回填）。
    pub ops: u64,
    /// 新条目 id 的会话内序号。
    seq: u64,
    /// 添加行草稿与选中色（EVENT_COLORS 下标）。
    pub draft: String,
    pub draft_color: usize,
    /// 标题行内编辑会话（条目 id + 草稿；None = 无）。
    pub edit: Option<(String, String)>,
    /// 待确认删除的日程 id（二段确认：Delete 进确认态，DeleteConfirm 才落变更）。
    pub delete_confirm: Option<String>,
    /// 编辑输入行按压标志（全局 Press 失焦提交判定，见 controls::inline_input）。
    pub press_inside: bool,
    /// 热力图拖拽会话（全局 Release 收尾）。
    heat_drag: Option<HeatDrag>,
    /// 空间写单飞（在途时变更入队，落地后续发；防并发读改写互相覆盖）。
    space_writing: bool,
    /// 空间在途期间排队的变更（按应用序，整批一次重放）。
    pending_ops: Vec<CalendarOp>,
    /// 本地活动密度缓存（Err = 上次装载失败，仅失效后重试防周期刷屏；空间模式不用）。
    pub activity: Option<Result<HashMap<i64, u32>, String>>,
    /// 活动密度装载在途。
    pub activity_loading: bool,
}

/// 日历视图消息。
#[derive(Debug, Clone)]
pub enum CalendarMsg {
    PrevMonth,
    NextMonth,
    Today,
    /// 热力图按压起点（松开翻月判定见 `finish_heat_drag`）。
    HeatPress(iced::window::Id),
    SelectDay(i64),
    DraftInput(String),
    DraftColor(usize),
    DraftSubmit,
    EditStart(String),
    EditInput(String),
    EditPressInside,
    EditCommit,
    CycleColor(String),
    /// 进入删除二段确认（行内「确认删除 / 取消」）。
    Delete(String),
    /// 确认删除（真正落变更）。
    DeleteConfirm(String),
    /// 取消删除确认。
    DeleteCancel,
    /// 日程装载回执（gen + 变更序双守卫）。
    Loaded(u64, u64, Result<Vec<CalendarItem>, String>),
    /// 变更落盘回执（本地读改写回读 / 空间 PATCH 后清单；Err 提示并回读真源）。
    Committed(u64, u64, Result<Vec<CalendarItem>, String>),
    /// 本地活动密度装载回执（gen 守卫）。
    ActivityLoaded(u64, Result<HashMap<i64, u32>, String>),
}

/// 日历消息处理。
pub fn on_msg(app: &mut App, msg: CalendarMsg) -> Task<Message> {
    match msg {
        CalendarMsg::PrevMonth => {
            app.calendar.month_offset -= 1;
            app.calendar.delete_confirm = None;
            Task::none()
        }
        CalendarMsg::NextMonth => {
            app.calendar.month_offset += 1;
            app.calendar.delete_confirm = None;
            Task::none()
        }
        CalendarMsg::Today => {
            app.calendar.month_offset = 0;
            app.calendar.selected = Some(day_key(crate::now_secs() as i64 * 1000));
            app.calendar.delete_confirm = None;
            Task::none()
        }
        CalendarMsg::HeatPress(window) => {
            // 无光标记录（该窗口尚未收到移动事件）时不开启会话，免得以默认原点当起点
            let start_x = app.cursors.get(&window).map(|pos| pos.x);
            app.calendar.heat_drag = start_x.map(|start_x| HeatDrag { window, start_x });
            Task::none()
        }
        CalendarMsg::SelectDay(day) => {
            app.calendar.selected = Some(day);
            app.calendar.delete_confirm = None;
            Task::none()
        }
        CalendarMsg::DraftInput(value) => {
            app.calendar.draft = value;
            Task::none()
        }
        CalendarMsg::DraftColor(idx) => {
            app.calendar.draft_color = idx;
            Task::none()
        }
        CalendarMsg::DraftSubmit => add_from_draft(app),
        CalendarMsg::EditStart(id) => start_edit(app, &id),
        CalendarMsg::EditInput(value) => {
            if let Some((_, draft)) = &mut app.calendar.edit {
                *draft = value;
            }
            Task::none()
        }
        CalendarMsg::EditPressInside => {
            app.calendar.press_inside = true;
            Task::none()
        }
        CalendarMsg::EditCommit => commit_edit(app),
        CalendarMsg::CycleColor(id) => cycle_color(app, &id),
        CalendarMsg::Delete(id) => {
            app.calendar.delete_confirm = Some(id);
            Task::none()
        }
        CalendarMsg::DeleteConfirm(id) => {
            app.calendar.delete_confirm = None;
            delete_item(app, &id)
        }
        CalendarMsg::DeleteCancel => {
            app.calendar.delete_confirm = None;
            Task::none()
        }
        CalendarMsg::Loaded(gen, rev, result) => on_loaded(app, gen, rev, result),
        CalendarMsg::Committed(gen, rev, result) => on_committed(app, gen, rev, result),
        CalendarMsg::ActivityLoaded(gen, result) => on_activity(app, gen, result),
    }
}

/// 热力图拖拽收尾：位移过阈值才翻月（左拖看下月、右拖看上月）；未过阈值视作普通按压。
pub fn finish_heat_drag(app: &mut App) {
    let Some(drag) = app.calendar.heat_drag.take() else { return };
    // 起点与终点都取本窗口的光标记录：窗口缺失记录（已关闭 / 从未移动）即放弃本次判定
    let Some(current) = app.cursors.get(&drag.window) else { return };
    let dx = current.x - drag.start_x;
    if dx.abs() < HEAT_DRAG_THRESHOLD {
        return;
    }
    app.calendar.month_offset += heat_drag_step(dx);
    app.calendar.delete_confirm = None;
}

/// 横向拖拽方向 → 月份增量。
fn heat_drag_step(dx: f32) -> i32 {
    if dx < 0.0 { 1 } else { -1 }
}

/// PersistTick 搭载：面板展示中按需装载日程与活动密度。
pub fn tick(app: &mut App) -> Task<Message> {
    if !repohistory_view::container_ready(app) || !layout_view::view_displayed(app, "calendar") {
        return Task::none();
    }
    let mut task = Task::none();
    if !app.calendar.ready && !app.calendar.loading {
        task = task.chain(reload(app));
    }
    if matches!(app.repo_kind, RepoKind::Local)
        && app.calendar.activity.is_none()
        && !app.calendar.activity_loading
    {
        task = task.chain(load_activity(app));
    }
    task
}

/// 活动密度缓存失效（记版回执 / 排除夹变更后；展示中下个 tick 重扫）。
pub fn invalidate_activity(app: &mut App) {
    app.calendar.activity = None;
}

/// 装载日程真源（本地 spawn_blocking 读文件 / 空间团队 meta 单键读）。
pub fn reload(app: &mut App) -> Task<Message> {
    app.calendar.loading = true;
    let (gen, rev) = (app.epoch(), app.calendar.ops);
    match &app.repo_kind {
        RepoKind::Space(session) => {
            let client = session.client.clone();
            let space_id = session.space_id.clone();
            Task::perform(
                async move { space_read(&client, &space_id).await },
                move |result| Message::Calendar(CalendarMsg::Loaded(gen, rev, result)),
            )
        }
        RepoKind::Local => {
            let state = app.vault.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let (root, _, _) = state.session_snapshot()?;
                        calendar::read_calendar(&root)
                    })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
                },
                move |result| Message::Calendar(CalendarMsg::Loaded(gen, rev, result)),
            )
        }
    }
}

/// 读团队 meta 的日程值（键缺失 = 尚无数据；读取失败向上传播，不得按空继续防误清）。
async fn space_read(client: &SpaceClient, space_id: &str) -> Result<Vec<CalendarItem>, String> {
    match client.team_meta_get(space_id, Some(META_KEY)).await {
        Ok(meta) => Ok(meta
            .values
            .get(META_KEY)
            .map(|raw| calendar::parse_calendar_json(raw))
            .unwrap_or_default()),
        Err(SpaceError::NotFound) => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// 读团队 meta 的日程值（写前严格口径：值损坏 = Err，防读改写 PATCH 清空服务端数据）。
async fn space_read_for_write(
    client: &SpaceClient,
    space_id: &str,
) -> Result<Vec<CalendarItem>, String> {
    match client.team_meta_get(space_id, Some(META_KEY)).await {
        Ok(meta) => match meta.values.get(META_KEY) {
            Some(raw) => calendar::parse_calendar_json_strict(raw),
            None => Ok(Vec::new()),
        },
        Err(SpaceError::NotFound) => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// 本地活动密度装载（版本计数 + 可见文件 mtime 计数，含排除夹过滤）。
fn load_activity(app: &mut App) -> Task<Message> {
    app.calendar.activity_loading = true;
    let gen = app.epoch();
    let state = app.vault.clone();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let (root, exclude, _) = state.session_snapshot()?;
                Ok::<_, String>(atelyx_core::history::activity_by_day(&root, &exclude))
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r)
        },
        move |result| Message::Calendar(CalendarMsg::ActivityLoaded(gen, result)),
    )
}

/// 写权限检查（viewer 只读 / 本地仓库未开 = 拦截并提示）。
fn ensure_writer(app: &mut App) -> bool {
    match &app.repo_kind {
        RepoKind::Space(s) if !s.role.can_write() => {
            app.notify(NoticeLevel::Warning, "只读身份无法修改日程");
            false
        }
        RepoKind::Local if app.vault.root().is_err() => {
            app.notify(NoticeLevel::Warning, "仓库未打开");
            false
        }
        _ => true,
    }
}

/// 应用变更到内存并分派落盘；无实际改动（重复提交 / 目标缺失）不发起写。
fn commit_op(app: &mut App, op: CalendarOp) -> Task<Message> {
    if !apply_op(&mut app.calendar.items, &op) {
        return Task::none();
    }
    app.calendar.ops += 1;
    match &app.repo_kind {
        RepoKind::Space(_) => {
            app.calendar.pending_ops.push(op);
            flush_space(app)
        }
        RepoKind::Local => write_local(app, op),
    }
}

/// 本地写：读改写作业进仓库写盘队列整体串行，重放基于最新磁盘，回执带落盘后清单。
fn write_local(app: &App, op: CalendarOp) -> Task<Message> {
    let Ok((root, _, _)) = app.vault.session_snapshot() else { return Task::none() };
    let queue = app.vault.writes.clone();
    let (gen, rev) = (app.epoch(), app.calendar.ops);
    Task::perform(
        async move {
            queue
                .bulk(move || {
                    let mut items = calendar::read_calendar(&root)?;
                    apply_op(&mut items, &op);
                    calendar::write_calendar(&root, &items)?;
                    calendar::read_calendar(&root)
                })
                .await
        },
        move |result| Message::Calendar(CalendarMsg::Committed(gen, rev, result)),
    )
}

/// 空间写单飞：在途时变更排队；整批 = 排队变更按序在最新服务端值上重放后一次 PATCH。
fn flush_space(app: &mut App) -> Task<Message> {
    if app.calendar.space_writing || app.calendar.pending_ops.is_empty() {
        return Task::none();
    }
    let Some(session) = space_actions::session_of(app) else { return Task::none() };
    app.calendar.space_writing = true;
    let ops = std::mem::take(&mut app.calendar.pending_ops);
    let (gen, rev) = (app.epoch(), app.calendar.ops);
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let mut items = space_read_for_write(&client, &space_id).await?;
            for op in &ops {
                apply_op(&mut items, op);
            }
            client
                .team_meta_patch(&space_id, &[(META_KEY.to_string(), calendar::calendar_json(&items)?)])
                .await
                .map_err(|e| e.to_string())?;
            Ok(items)
        },
        move |result| Message::Calendar(CalendarMsg::Committed(gen, rev, result)),
    )
}

fn add_from_draft(app: &mut App) -> Task<Message> {
    let title = app.calendar.draft.trim().to_string();
    let Some(day) = app.calendar.selected else { return Task::none() };
    if title.is_empty() || !ensure_writer(app) {
        return Task::none();
    }
    app.calendar.seq += 1;
    let now_ms = crate::now_secs() as i64 * 1000;
    let item = CalendarItem {
        id: next_item_id(now_ms, app.calendar.seq),
        date: calendar_grid::date_string(day),
        title,
        note: None,
        color: Some(color_hex(app.calendar.draft_color)),
        created_at: now_ms,
    };
    app.calendar.draft.clear();
    commit_op(app, CalendarOp::Add(item))
}

/// 条目 id = 毫秒时间戳 + 会话序号 + 随机 hex 后缀：双客户端同毫秒同序号生成不同 id，不互替。
fn next_item_id(now_ms: i64, seq: u64) -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState 自带进程级随机种子，混散时间戳与序号取低位即得短随机段
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(now_ms as u64);
    hasher.write_u64(seq);
    format!("ev-{now_ms}-{seq}-{:06x}", hasher.finish() as u32 & 0xffffff)
}

/// 进入标题行内编辑（切行先提交在编草稿；发起按压不算失焦），并挂输入焦点。
fn start_edit(app: &mut App, id: &str) -> Task<Message> {
    if !ensure_writer(app) {
        return Task::none();
    }
    let commit = commit_edit(app);
    let Some(item) = app.calendar.items.iter().find(|it| it.id == id) else { return commit };
    app.calendar.edit = Some((id.to_string(), item.title.clone()));
    app.calendar.press_inside = true;
    commit.chain(focus(iced::widget::Id::new(EDIT_INPUT_ID)))
}

/// 提交标题编辑（Enter / 失焦同路径）：空草稿或与原值一致 = 仅收会话。
fn commit_edit(app: &mut App) -> Task<Message> {
    let Some((id, draft)) = app.calendar.edit.take() else { return Task::none() };
    let title = draft.trim().to_string();
    if title.is_empty() {
        return Task::none();
    }
    commit_op(app, CalendarOp::SetTitle(id, title))
}

fn cycle_color(app: &mut App, id: &str) -> Task<Message> {
    if !ensure_writer(app) {
        return Task::none();
    }
    commit_op(app, CalendarOp::CycleColor(id.to_string()))
}

fn delete_item(app: &mut App, id: &str) -> Task<Message> {
    if !ensure_writer(app) {
        return Task::none();
    }
    if app.calendar.edit.as_ref().is_some_and(|(eid, _)| eid == id) {
        app.calendar.edit = None;
    }
    commit_op(app, CalendarOp::Remove(id.to_string()))
}

/// 装载回执落地（gen + 变更序双守卫：过期读不回填，在途回写自会收敛）。
/// 读失败按已装载空清单继续（提示一次，不周期重试刷屏；空间经 meta-changed 重载恢复）。
fn on_loaded(
    app: &mut App,
    gen: u64,
    rev: u64,
    result: Result<Vec<CalendarItem>, String>,
) -> Task<Message> {
    app.calendar.loading = false;
    if gen != app.epoch() {
        return Task::none();
    }
    match result {
        Ok(items) => {
            if rev == app.calendar.ops {
                app.calendar.items = items;
                app.calendar.ready = true;
            }
        }
        Err(e) => {
            app.calendar.ready = true;
            app.notify(NoticeLevel::Error, format!("读取日程失败：{e}"));
        }
    }
    Task::none()
}

/// 落盘回执落地：成功 = 回读清单收敛内存（覆盖外部改动 / 合流远端条目）；
/// 失败 = 提示 + 回读真源（被拒变更不再显示，排队变更一并放弃）。
fn on_committed(app: &mut App, gen: u64, rev: u64, result: Result<Vec<CalendarItem>, String>) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    match result {
        Ok(items) => {
            app.calendar.space_writing = false;
            if rev == app.calendar.ops {
                app.calendar.items = items;
                app.calendar.ready = true;
            }
            flush_space(app)
        }
        Err(e) => {
            app.calendar.space_writing = false;
            app.calendar.pending_ops.clear();
            app.notify(NoticeLevel::Error, format!("日程保存失败：{e}"));
            reload(app)
        }
    }
}

fn on_activity(
    app: &mut App,
    gen: u64,
    result: Result<HashMap<i64, u32>, String>,
) -> Task<Message> {
    app.calendar.activity_loading = false;
    if gen != app.epoch() {
        return Task::none();
    }
    match result {
        Ok(counts) => app.calendar.activity = Some(Ok(counts)),
        Err(e) => {
            // 缓存 Err 停止周期重试（防 800ms tick 刷屏），失效后由 tick 重扫
            app.notify(NoticeLevel::Error, format!("活动统计失败：{e}"));
            app.calendar.activity = Some(Err(e));
        }
    }
    Task::none()
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

    #[test]
    fn add_upserts_and_set_title_updates_existing_only() {
        let mut items = vec![item("a", "旧")];
        assert!(apply_op(&mut items, &CalendarOp::Add(item("b", "新"))));
        assert_eq!(items.len(), 2);
        assert!(apply_op(&mut items, &CalendarOp::SetTitle("a".into(), "改".into())));
        assert_eq!(items.iter().find(|it| it.id == "a").unwrap().title, "改");
        // 相同标题与缺失条目 = 无改动
        assert!(!apply_op(&mut items, &CalendarOp::SetTitle("a".into(), "改".into())));
        assert!(!apply_op(&mut items, &CalendarOp::SetTitle("zz".into(), "x".into())));
    }

    #[test]
    fn remove_reports_change_only_when_present() {
        let mut items = vec![item("a", "x")];
        assert!(apply_op(&mut items, &CalendarOp::Remove("a".into())));
        assert!(items.is_empty());
        assert!(!apply_op(&mut items, &CalendarOp::Remove("a".into())));
    }

    #[test]
    fn cycle_color_walks_palette_and_wraps() {
        let mut items = vec![item("a", "x")];
        for expected in 0..pal::EVENT_COLORS.len() {
            assert!(apply_op(&mut items, &CalendarOp::CycleColor("a".into())));
            assert_eq!(
                items[0].color.as_deref(),
                Some(color_hex(expected).as_str())
            );
        }
        // 第 8 次回到首色
        assert!(apply_op(&mut items, &CalendarOp::CycleColor("a".into())));
        assert_eq!(items[0].color.as_deref(), Some(color_hex(0).as_str()));
        assert!(!apply_op(&mut items, &CalendarOp::CycleColor("zz".into())));
    }

    #[test]
    fn heat_drag_step_flips_toward_drag() {
        // 左拖 = 看下月，右拖 = 看上月
        assert_eq!(heat_drag_step(-HEAT_DRAG_THRESHOLD), 1);
        assert_eq!(heat_drag_step(HEAT_DRAG_THRESHOLD), -1);
    }

    #[test]
    fn color_hex_index_round_trip_and_foreign_hex() {
        assert_eq!(color_index(&color_hex(3)), Some(3));
        // 大小写不敏感
        assert_eq!(color_index(&color_hex(5).to_uppercase()), Some(5));
        // 色板外（默认强调色等）不在循环内：下次循环从首色开始
        assert_eq!(color_index("#e0a94e"), None);
    }
}
