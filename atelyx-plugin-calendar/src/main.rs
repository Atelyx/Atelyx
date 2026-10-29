//! 日历插件入口：注册面板贡献，按月浏览与编辑当前仓库的日程（本地仓库与协作空间同口径）。

mod date;
mod model;
mod ui;

use std::sync::Mutex;

use atelyx_plugin_protocol::event::{self, PublishParams};
use atelyx_plugin_protocol::frame::FrameInputParams;
use atelyx_plugin_protocol::method::{ContributionSlot, PaddingSpec, SurfaceAttachedParams};
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{run_stdio, Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use crate::model::{Action, Model};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "com.atelyx.calendar";

struct CalendarPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
}

impl CalendarPlugin {
    /// 用宿主回读的清单替换本地状态：日程真源在宿主，插件只缓存渲染用的一份。
    async fn refresh(&self, ctx: &Context) {
        match ctx.calendar_list().await {
            Ok(items) => self.model.lock().unwrap().replace(items),
            Err(error) => self.model.lock().unwrap().fail(format!("读取日程失败：{error}")),
        }
    }

    async fn apply_theme(&self, ctx: &Context) {
        match ctx.query_theme().await {
            Ok(theme) => self.model.lock().unwrap().set_micro(theme.font_sizes.micro),
            Err(error) => eprintln!("取宿主主题失败，按缺省字号绘制：{error}"),
        }
    }

    async fn render(&self, ctx: &Context) {
        let snapshot = self.model.lock().unwrap().snapshot();
        let mut frame_ui = self.ui.lock().await;
        if let Err(error) = frame_ui
            .render(ctx, model::PANEL, move |frame| ui::frame(frame, &snapshot))
            .await
        {
            eprintln!("提交日历面板失败：{error}");
        }
    }

    async fn run(&self, ctx: &Context, action: Action) {
        match action {
            Action::Add => {
                let params = {
                    let mut model = self.model.lock().unwrap();
                    match model.add_params() {
                        Ok(params) => params,
                        Err(message) => {
                            model.fail(message);
                            return;
                        }
                    }
                };
                match ctx.calendar_add(&params).await {
                    Ok(_) => {
                        self.model.lock().unwrap().clear_draft();
                        self.refresh(ctx).await;
                    }
                    Err(error) => self.model.lock().unwrap().fail(format!("添加失败：{error}")),
                }
            }
            Action::Delete(id) => match ctx.calendar_remove(&id).await {
                Ok(()) => {
                    self.model.lock().unwrap().cancel_delete();
                    self.refresh(ctx).await;
                }
                Err(error) => self.model.lock().unwrap().fail(format!("删除失败：{error}")),
            },
        }
    }
}

/// 网格与条目上的点击：日格选中、删除两段式（先点删除进入确认，再点确认才删）。
fn collect_action(model: &mut Model, id: &str, actions: &mut Vec<Action>) {
    let Some((prefix, rest)) = id.split_once(':') else {
        return;
    };
    match prefix {
        "day" => {
            if let Some(day) = date::parse_date(rest) {
                model.select(day);
            }
        }
        "del" => model.request_delete(rest),
        "delcancel" => model.cancel_delete(),
        "delok" => actions.push(Action::Delete(rest.to_string())),
        _ => {}
    }
}

#[async_trait::async_trait]
impl PluginHandler for CalendarPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(
                ContributionSlot::Panel,
                model::PANEL,
                "日历",
                Some("calendar-days"),
                Some(PaddingSpec::All(12.0)),
            )
            .await
        {
            eprintln!("注册日历面板失败：{error}");
        }
        if let Err(error) = ctx.subscribe(event::name::CALENDAR_CHANGED).await {
            eprintln!("订阅日程变更失败：{error}");
        }
        self.apply_theme(ctx).await;
        self.refresh(ctx).await;
        self.render(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != model::PANEL {
            return;
        }
        // 重挂是重取主题的唯一时机（控件层按面缓存主题）；焦点与滚动位置随之重置
        *self.ui.lock().await = FrameUi::new();
        self.apply_theme(ctx).await;
        self.refresh(ctx).await;
        self.render(ctx).await;
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        if params.name == event::name::CALENDAR_CHANGED {
            self.refresh(ctx).await;
            self.render(ctx).await;
        }
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != model::PANEL {
            return;
        }
        let draft = self.model.lock().unwrap().draft().to_string();
        let events = {
            let mut frame_ui = self.ui.lock().await;
            frame_ui
                .event(ctx, &params.input, |id| (id == "draft").then(|| draft.clone()))
                .await
        };
        let mut actions = Vec::new();
        {
            let mut model = self.model.lock().unwrap();
            for event in events {
                match event {
                    UiEvent::Click { id } => match id.as_str() {
                        "prev-month" => model.shift_month(-1),
                        "next-month" => model.shift_month(1),
                        "today" => model.show_today(),
                        "add" => actions.push(Action::Add),
                        other => collect_action(&mut model, other, &mut actions),
                    },
                    UiEvent::Text { id, value } if id == "draft" => model.set_draft(value),
                    UiEvent::Submit { id } if id == "draft" => actions.push(Action::Add),
                    _ => {}
                }
            }
        }
        for action in actions {
            self.run(ctx, action).await;
        }
        self.render(ctx).await;
    }
}

#[tokio::main]
async fn main() {
    let plugin = CalendarPlugin {
        model: Mutex::new(Model::new(model::DEFAULT_MICRO)),
        ui: AsyncMutex::new(FrameUi::new()),
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
