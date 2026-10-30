//! 日历插件入口：面板贡献与 `calendar` 服务共用同一份日程真源（本地文件 / 空间团队 meta），
//! 整面帧只做展示与交互编排，写盘一律「读真源 → 应用变更 → 写回 → 读回」。

mod date;
mod model;
mod ops;
mod store;
mod ui;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::Value;

use atelyx_plugin_protocol::event::{
    self, FileChangedPayload, PublishParams, SpaceMetaChangedPayload, VaultSwitchPayload,
};
use atelyx_plugin_protocol::frame::FrameInputParams;
use atelyx_plugin_protocol::method::{
    calendar, CalendarAddParams, CalendarIdParams, CalendarUpdateParams, ContributionSlot,
    SurfaceAttachedParams, SurfaceDetachedParams, MAX_CALENDAR_TITLE_CHARS,
};
use atelyx_plugin_protocol::service::HostCallParams;
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use crate::model::{Model, Palette, PANEL};
use crate::ops::Mutation;
use crate::store::Backend;

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "com.atelyx.calendar";
/// 本插件开放的服务方法（调用方经 `service.call` 传 `service = calendar`）。
const SERVICE_METHODS: [&str; 4] = [calendar::LIST, calendar::ADD, calendar::UPDATE, calendar::REMOVE];
/// 热力图横向拖拽翻月的最小位移（左拖看下月）。
const HEAT_DRAG_PX: f32 = 40.0;

struct CalendarPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
    /// 新建条目 id 的会话内序号。
    seq: AtomicU64,
    /// 面板面是否已挂载：未挂载时不重载数据也不提交帧（挂载时统一重取）。
    attached: Mutex<bool>,
}

impl CalendarPlugin {
    fn backend(&self) -> Result<Backend, String> {
        self.model.lock().unwrap().backend().ok_or_else(|| "仓库未打开".to_string())
    }

    fn is_attached(&self) -> bool {
        *self.attached.lock().unwrap()
    }

    async fn apply_theme(&self, ctx: &Context) {
        match ctx.query_theme().await {
            Ok(theme) => self.model.lock().unwrap().set_palette(Palette::from_theme(&theme)),
            Err(error) => eprintln!("取宿主主题失败，按缺省样式绘制：{error}"),
        }
    }

    /// 展示读：失败不回退成空清单，错误原样进面板。
    async fn reload_items(&self, ctx: &Context) {
        let Ok(backend) = self.backend() else { return };
        match store::load(ctx, &backend).await {
            Ok(items) => self.model.lock().unwrap().replace(items),
            Err(error) => self.model.lock().unwrap().fail(error),
        }
    }

    async fn load_activity(&self, ctx: &Context) {
        self.model.lock().unwrap().set_activity_loading();
        let result = match ctx.history_activity().await {
            Ok(days) => Ok(days.into_iter().map(|day| (day.day, day.count)).collect()),
            Err(error) => Err(error.to_string()),
        };
        self.model.lock().unwrap().set_activity(result);
    }

    async fn load_history(&self, ctx: &Context, day: i64) {
        let key = date::date_string(day);
        let result = ctx.history_day(&key).await.map_err(|error| error.to_string());
        self.model.lock().unwrap().set_history(day, result);
    }

    /// 一次写入：真源最新内容上应用变更并整文件写回，读回清单即新的展示真源。
    /// `Ok(None)` = 变更没有实际改动，不落盘；`Unrefreshed` = 已落盘但读回失败。
    async fn commit(&self, ctx: &Context, mutation: Mutation) -> Result<Option<Value>, CommitError> {
        let backend = self.backend().map_err(CommitError::Rejected)?;
        if let Some(reason) = self.model.lock().unwrap().blocked_reason() {
            return Err(CommitError::Rejected(reason.to_string()));
        }
        let mut items = store::load_for_write(ctx, &backend).await.map_err(CommitError::Rejected)?;
        let palette = self.model.lock().unwrap().palette().event_colors.clone();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let Some(value) =
            ops::apply(&mut items, &mutation, seq, date::now_ms(), &palette).map_err(CommitError::Rejected)?
        else {
            return Ok(None);
        };
        store::save(ctx, &backend, &items).await.map_err(CommitError::Rejected)?;
        match store::load(ctx, &backend).await {
            Ok(readback) => {
                self.model.lock().unwrap().replace(readback);
                Ok(Some(value))
            }
            Err(error) => Err(CommitError::Unrefreshed(error)),
        }
    }

    /// 服务调用的写入：写已生效时按成功返回（报错会招致调用方重试造成重复写入），
    /// 读回失败的原因只进日志。
    async fn commit_service(&self, ctx: &Context, mutation: Mutation) -> Result<Value, String> {
        match self.commit(ctx, mutation).await {
            Ok(value) => Ok(empty_when_none(value)),
            Err(CommitError::Unrefreshed(error)) => {
                eprintln!("日程已写入但读回失败：{error}");
                Ok(serde_json::json!({}))
            }
            Err(CommitError::Rejected(error)) => Err(error),
        }
    }

    async fn render(&self, ctx: &Context) {
        if !self.is_attached() {
            return;
        }
        let focus = self.model.lock().unwrap().take_focus_edit();
        if let Some((id, draft)) = focus {
            self.ui.lock().await.focus_input(&format!("edit:{id}"), &draft);
        }
        let snapshot = self.model.lock().unwrap().snapshot();
        let mut frame_ui = self.ui.lock().await;
        if let Err(error) = frame_ui
            .render(ctx, PANEL, move |frame| ui::frame(frame, &snapshot))
            .await
        {
            eprintln!("提交日历面板失败：{error}");
        }
    }

    async fn on_switch(&self, ctx: &Context, payload: &Value) {
        let backend = match serde_json::from_value::<VaultSwitchPayload>(payload.clone()) {
            Ok(VaultSwitchPayload::Local { .. }) => Backend::Local,
            Ok(VaultSwitchPayload::Space { can_write, .. }) => Backend::Space { can_write },
            Err(error) => {
                eprintln!("仓库切换载荷不可解析：{error}");
                return;
            }
        };
        self.model.lock().unwrap().set_backend(backend);
        if !self.is_attached() {
            return;
        }
        self.reload_items(ctx).await;
        self.load_activity(ctx).await;
        self.render(ctx).await;
    }

    async fn on_file_changed(&self, ctx: &Context, payload: &Value) {
        if !self.is_attached() {
            return;
        }
        let payload = match serde_json::from_value::<FileChangedPayload>(payload.clone()) {
            Ok(payload) => payload,
            Err(error) => {
                eprintln!("文件变更载荷不可解析：{error}");
                return;
            }
        };
        let is_calendar_file = |path: &str| path == store::CALENDAR_FILE;
        let touches_items =
            is_calendar_file(&payload.path) || payload.old_path.as_deref().is_some_and(is_calendar_file);
        // 仓库内容版本落在 `.atelyx/` 之外，热力图与当日历史都随之变化
        let touches_activity = !payload.path.starts_with(".atelyx/")
            || payload.old_path.as_deref().is_some_and(|old| !old.starts_with(".atelyx/"));
        if !touches_items && !touches_activity {
            return;
        }
        if touches_items {
            self.reload_items(ctx).await;
        }
        if touches_activity {
            self.load_activity(ctx).await;
            let selected = self.model.lock().unwrap().selected();
            if let Some(day) = selected {
                self.load_history(ctx, day).await;
            }
        }
        self.render(ctx).await;
    }

    async fn on_space_meta(&self, ctx: &Context, payload: &Value) {
        if !self.is_attached() {
            return;
        }
        let payload = match serde_json::from_value::<SpaceMetaChangedPayload>(payload.clone()) {
            Ok(payload) => payload,
            Err(error) => {
                eprintln!("团队元数据变更载荷不可解析：{error}");
                return;
            }
        };
        if payload.key != store::META_KEY {
            return;
        }
        self.reload_items(ctx).await;
        self.render(ctx).await;
    }

    /// `calendar.*` 服务调用：入参先校验，写操作按容器权限拦截。
    async fn call_service(&self, ctx: &Context, method: &str, params: Value) -> Result<Value, String> {
        match method {
            calendar::LIST => {
                let backend = self.backend()?;
                let items = store::load(ctx, &backend).await?;
                self.model.lock().unwrap().replace(items.clone());
                serde_json::to_value(ops::list_result(&items)).map_err(|error| error.to_string())
            }
            calendar::ADD => {
                let params: CalendarAddParams = parse_params(method, params)?;
                params.validate()?;
                validate_service_date(&params.date)?;
                self.commit_service(ctx, Mutation::Add(params)).await
            }
            calendar::UPDATE => {
                let params: CalendarUpdateParams = parse_params(method, params)?;
                params.validate()?;
                if let Some(date) = &params.date {
                    validate_service_date(date)?;
                }
                self.commit_service(ctx, Mutation::Update(params)).await
            }
            calendar::REMOVE => {
                let params: CalendarIdParams = parse_params(method, params)?;
                params.validate()?;
                self.commit_service(ctx, Mutation::Remove(params.id)).await
            }
            other => Err(format!("未提供该方法：{}.{other}", calendar::SERVICE)),
        }
    }
}

/// 写入失败：`Rejected` = 未落盘；`Unrefreshed` = 已落盘但读回失败（变更生效，展示仍是旧清单）。
enum CommitError {
    Rejected(String),
    Unrefreshed(String),
}

fn parse_params<T: serde::de::DeserializeOwned>(method: &str, params: Value) -> Result<T, String> {
    serde_json::from_value(params)
        .map_err(|error| format!("{}.{method} 参数不合法：{error}", calendar::SERVICE))
}

/// 服务入参的日期必须在公历上真实存在：形状校验挡不住 `2026-02-31`，
/// 落盘后这种条目不会被网格展示。
fn validate_service_date(date: &str) -> Result<(), String> {
    crate::date::parse_date(date)
        .map(|_| ())
        .ok_or_else(|| format!("日期不存在：{date}"))
}

fn empty_when_none(value: Option<Value>) -> Value {
    value.unwrap_or_else(|| serde_json::json!({}))
}

/// 点开了当日历史里的一行：由宿主打开该文件的历史弹层。
struct HistoryOpen {
    kind: String,
    file: String,
}

/// 点击落在行内编辑的输入框上时不算「切走」，其余点击都先提交在编草稿。
fn release_edit(model: &mut Model, id: &str, mutations: &mut Vec<Mutation>) {
    let Some(target) = model.edit_target() else { return };
    if id == format!("edit:{target}") {
        return;
    }
    commit_edit(model, mutations);
}

/// 提交标题编辑：空草稿或与原值一致只收会话，不发写请求。
fn commit_edit(model: &mut Model, mutations: &mut Vec<Mutation>) {
    let Some((id, draft)) = model.take_edit() else { return };
    let title = draft.trim();
    if title.is_empty() {
        return;
    }
    if title.chars().count() > MAX_CALENDAR_TITLE_CHARS {
        model.fail(format!("标题不得超过 {MAX_CALENDAR_TITLE_CHARS} 字"));
        return;
    }
    mutations.push(Mutation::SetTitle(id, title.to_string()));
}

fn push_add(model: &mut Model, mutations: &mut Vec<Mutation>) {
    if let Some(reason) = model.blocked_reason() {
        model.fail(reason.to_string());
        return;
    }
    match model.add_params() {
        Ok(params) => mutations.push(Mutation::Add(params)),
        Err(message) => model.fail(message),
    }
}

fn handle_click(
    model: &mut Model,
    id: &str,
    mutations: &mut Vec<Mutation>,
    open_history: &mut Option<HistoryOpen>,
) {
    let writable = model.writable();
    if id == "prev-month" {
        model.shift_month(-1);
    } else if id == "next-month" {
        model.shift_month(1);
    } else if id == "today" {
        model.show_today();
    } else if id == "add" {
        push_add(model, mutations);
    } else if let Some(rest) = id.strip_prefix("day:") {
        if let Some(day) = date::parse_date(rest) {
            model.select(day);
        }
    } else if let Some(rest) = id.strip_prefix("hist:") {
        if let Ok(index) = rest.parse::<usize>() {
            *open_history = model.history_entry(index).map(|entry| HistoryOpen {
                kind: entry.kind,
                file: entry.file,
            });
        }
    } else if let Some(rest) = id.strip_prefix("cycle:") {
        if writable {
            mutations.push(Mutation::CycleColor(rest.to_string()));
        }
    } else if let Some(rest) = id.strip_prefix("delok:") {
        model.cancel_delete();
        if writable {
            mutations.push(Mutation::Remove(rest.to_string()));
        }
    } else if id.starts_with("delcancel:") {
        model.cancel_delete();
    } else if let Some(rest) = id.strip_prefix("del:") {
        model.request_delete(rest);
    } else if let Some(rest) = id.strip_prefix("edit:") {
        if writable && !model.editing(rest) {
            model.begin_edit(rest);
        }
    } else if let Some(rest) = id.strip_prefix("color:") {
        if let Ok(index) = rest.parse::<usize>() {
            model.set_draft_color(index);
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for CalendarPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(
                ContributionSlot::Panel,
                PANEL,
                "日历",
                Some("calendar-days"),
                None,
            )
            .await
        {
            eprintln!("注册日历面板失败：{error}");
        }
        if let Err(error) = ctx.register_service(calendar::SERVICE, &SERVICE_METHODS).await {
            eprintln!("登记日历服务失败：{error}");
        }
        for name in [
            event::name::VAULT_SWITCH,
            event::name::VAULT_FILE_CHANGED,
            event::name::SPACE_META_CHANGED,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                eprintln!("订阅 {name} 失败：{error}");
            }
        }
        self.apply_theme(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != PANEL {
            return;
        }
        *self.attached.lock().unwrap() = true;
        // 重挂是重取主题的唯一时机（控件层按面缓存主题）；焦点与滚动位置随之重置
        *self.ui.lock().await = FrameUi::new();
        self.apply_theme(ctx).await;
        self.reload_items(ctx).await;
        self.load_activity(ctx).await;
        let selected = self.model.lock().unwrap().selected();
        if let Some(day) = selected {
            self.load_history(ctx, day).await;
        }
        self.render(ctx).await;
    }

    async fn on_surface_detached(&self, _ctx: &Context, params: SurfaceDetachedParams) {
        if params.surface_id == PANEL {
            *self.attached.lock().unwrap() = false;
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        match params.name.as_str() {
            event::name::VAULT_SWITCH => self.on_switch(ctx, &params.payload).await,
            event::name::VAULT_FILE_CHANGED => self.on_file_changed(ctx, &params.payload).await,
            event::name::SPACE_META_CHANGED => self.on_space_meta(ctx, &params.payload).await,
            _ => {}
        }
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL {
            return;
        }
        let events = {
            let mut frame_ui = self.ui.lock().await;
            frame_ui
                .event(ctx, &params.input, |id| {
                    let model = self.model.lock().unwrap();
                    if id == "draft" {
                        Some(model.draft().to_string())
                    } else if id == format!("edit:{}", model.edit_target().unwrap_or_default()) {
                        model.edit_draft()
                    } else {
                        None
                    }
                })
                .await
        };
        let mut mutations = Vec::new();
        let mut open_history = None;
        {
            let mut model = self.model.lock().unwrap();
            for event in events {
                match event {
                    UiEvent::Click { id } => {
                        release_edit(&mut model, &id, &mut mutations);
                        handle_click(&mut model, &id, &mut mutations, &mut open_history);
                    }
                    UiEvent::RightClick { id } | UiEvent::DragStart { id } => {
                        release_edit(&mut model, &id, &mut mutations);
                    }
                    UiEvent::DragEnd { id, dx, .. } => {
                        if id.starts_with("heat:") && dx.abs() >= HEAT_DRAG_PX {
                            model.shift_month(if dx < 0.0 { 1 } else { -1 });
                        }
                    }
                    UiEvent::Text { id, value } if id == "draft" => model.set_draft(value),
                    UiEvent::Text { id, value } if id.starts_with("edit:") => model.set_edit_draft(value),
                    UiEvent::Submit { id } if id == "draft" => push_add(&mut model, &mut mutations),
                    UiEvent::Submit { id } if id.starts_with("edit:") => {
                        commit_edit(&mut model, &mut mutations)
                    }
                    UiEvent::Focus { focused: false, id } if id.starts_with("edit:") => {
                        commit_edit(&mut model, &mut mutations)
                    }
                    UiEvent::Cancel { id } if id.starts_with("edit:") => model.cancel_edit(),
                    _ => {}
                }
            }
        }
        if let Some(entry) = open_history {
            if let Err(error) = ctx.history_open(&entry.kind, &entry.file).await {
                self.model.lock().unwrap().fail(format!("打开历史失败：{error}"));
            }
        }
        let mut added = false;
        for mutation in mutations {
            let is_add = matches!(&mutation, Mutation::Add(_));
            let error_prefix = match &mutation {
                Mutation::Add(_) => "添加失败：",
                Mutation::Remove(_) => "删除失败：",
                Mutation::SetTitle(..) | Mutation::CycleColor(_) | Mutation::Update(_) => {
                    "日程保存失败："
                }
            };
            match self.commit(ctx, mutation).await {
                Ok(_) => added |= is_add,
                Err(CommitError::Unrefreshed(error)) => {
                    // 写已经生效：按成功处理，读回失败单独提示（下一次重载会纠正展示）
                    added |= is_add;
                    self.model
                        .lock()
                        .unwrap()
                        .fail(format!("{error_prefix}已写入，但重新读取失败：{error}"));
                }
                Err(CommitError::Rejected(error)) => {
                    self.model.lock().unwrap().fail(format!("{error_prefix}{error}"));
                    self.reload_items(ctx).await;
                    break;
                }
            }
        }
        if added {
            self.model.lock().unwrap().clear_draft();
        }
        let pending = self.model.lock().unwrap().pending_history_day();
        if let Some(day) = pending {
            self.load_history(ctx, day).await;
        }
        self.render(ctx).await;
    }

    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        let result = if params.service != calendar::SERVICE {
            Err(format!("未提供该服务：{}", params.service))
        } else {
            self.call_service(ctx, &params.method, params.params).await
        };
        let response = match result {
            Ok(value) => ctx.service_result(&params.call_id, value).await,
            Err(error) => ctx.service_error(&params.call_id, &error).await,
        };
        if let Err(error) = response {
            eprintln!("回报服务调用结果失败：{error}");
        }
        self.render(ctx).await;
    }
}

#[tokio::main]
async fn main() {
    let plugin = CalendarPlugin {
        model: Mutex::new(Model::new()),
        ui: AsyncMutex::new(FrameUi::new()),
        seq: AtomicU64::new(0),
        attached: Mutex::new(false),
    };
    let info = PluginInfo {
        id: PLUGIN_ID.into(),
        name: "日历".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    if let Err(error) = run_stdio(info, plugin).await {
        eprintln!("日历插件退出：{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随包安装读取的是 `plugin.json`，与进程自报的身份必须一致。
    #[test]
    fn manifest_matches_the_crate_identity() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    }
}
