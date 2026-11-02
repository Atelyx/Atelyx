//! 仓库历史插件入口：面板贡献只读宿主的版本聚合（`history.groups`），
//! 卡片点击让宿主打开该文件的历史弹层（版本浏览与回滚都在宿主）。

mod model;
mod view;

use std::sync::Mutex;

use atelyx_plugin_protocol::event::{self, PublishParams};
use atelyx_plugin_protocol::frame::FrameInputParams;
use atelyx_plugin_protocol::method::{
    ContributionSlot, SurfaceAttachedParams, SurfaceDetachedParams,
};
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use crate::model::Model;

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.history";
/// 面板贡献 id（同时是面标识 `plugin:atelyx.history:repohistory` 的贡献段）。
const PANEL: &str = "repohistory";
/// 重试按钮的控件 id。
const RETRY_ID: &str = "retry";

struct HistoryPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
    /// 面板面是否已挂载：未挂载时不取数也不提交帧（挂载时统一重取）。
    attached: Mutex<bool>,
}

impl HistoryPlugin {
    fn new() -> Self {
        Self {
            model: Mutex::new(Model::new()),
            ui: AsyncMutex::new(FrameUi::new()),
            attached: Mutex::new(false),
        }
    }

    fn is_attached(&self) -> bool {
        *self.attached.lock().unwrap()
    }

    async fn apply_theme(&self, ctx: &Context) {
        match ctx.query_theme().await {
            Ok(theme) => self.model.lock().unwrap().set_radii(theme.radii),
            Err(error) => ctx.log_error(&format!("取宿主主题失败，按缺省圆角绘制：{error}")),
        }
    }

    /// 取聚合：`refresh` = 丢弃宿主缓存重扫（失败后的重试用），日常事件只吃缓存。
    async fn reload(&self, ctx: &Context, refresh: bool) {
        match ctx.history_groups(refresh).await {
            Ok(result) => self.model.lock().unwrap().apply(result),
            Err(error) => {
                ctx.log_error(&format!("取历史分组失败：{error}"));
                self.model.lock().unwrap().fail(error.to_string());
            }
        }
    }

    async fn render(&self, ctx: &Context) {
        if !self.is_attached() {
            return;
        }
        let snapshot = self.model.lock().unwrap().clone();
        let mut ui = self.ui.lock().await;
        if let Err(error) = ui
            .render(ctx, PANEL, move |frame| view::frame(frame, &snapshot))
            .await
        {
            ctx.log_error(&format!("提交仓库历史面板失败：{error}"));
        }
    }

    /// 点开一张卡片：按世代与行号取该分组让宿主打开历史弹层；旧帧上的点击作废。
    async fn open_history(&self, ctx: &Context, gen: u64, index: usize) {
        let target = {
            let model = self.model.lock().unwrap();
            if model.gen != gen {
                return;
            }
            model.groups.get(index).map(|group| (group.kind.clone(), group.file.clone()))
        };
        let Some((kind, file)) = target else { return };
        if let Err(error) = ctx.history_open(&kind, &file).await {
            ctx.log_error(&format!("打开历史弹层失败：{error}"));
            self.model.lock().unwrap().fail(format!("打开历史失败：{error}"));
        }
    }
}

#[async_trait::async_trait]
impl PluginHandler for HistoryPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "仓库历史", Some("history"), None)
            .await
        {
            ctx.log_error(&format!("注册仓库历史面板失败：{error}"));
        }
        if let Err(error) = ctx.subscribe(event::name::HISTORY_CHANGED).await {
            ctx.log_error(&format!("订阅 {} 失败：{error}", event::name::HISTORY_CHANGED));
        }
        self.apply_theme(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != PANEL {
            return;
        }
        *self.attached.lock().unwrap() = true;
        // 重挂只重取控件层主题，保留宿主已上报的尺寸（尺寸变了宿主会再发 resize）
        self.ui.lock().await.refresh_theme();
        self.apply_theme(ctx).await;
        self.reload(ctx, false).await;
        self.render(ctx).await;
    }

    async fn on_surface_detached(&self, _ctx: &Context, params: SurfaceDetachedParams) {
        if params.surface_id == PANEL {
            *self.attached.lock().unwrap() = false;
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        if params.name != event::name::HISTORY_CHANGED || !self.is_attached() {
            return;
        }
        // 宿主已丢弃聚合缓存：这里按缺省口径重取即拿到新聚合
        self.reload(ctx, false).await;
        self.render(ctx).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL {
            return;
        }
        let events = {
            let mut frame_ui = self.ui.lock().await;
            frame_ui.event(ctx, &params.input, |_| None).await
        };
        let mut click = None;
        let mut retry = false;
        for event in events {
            if let UiEvent::Click { id } = event {
                if id == RETRY_ID {
                    retry = true;
                } else if let Some(target) = parse_open_id(&id) {
                    click = Some(target);
                }
            }
        }
        if retry {
            self.model.lock().unwrap().error = None;
            self.reload(ctx, true).await;
        }
        if let Some((gen, index)) = click {
            self.open_history(ctx, gen, index).await;
        }
        // 尺寸与悬停 / 按下的状态都由这一帧体现：任何输入之后都要交出按当前尺寸排的帧
        self.render(ctx).await;
    }
}

/// 解析卡片命中区 id（`open:<分组世代>:<行号>`）；形态不符返回 `None`。
fn parse_open_id(id: &str) -> Option<(u64, usize)> {
    let rest = id.strip_prefix("open:")?;
    let (gen, index) = rest.split_once(':')?;
    Some((gen.parse().ok()?, index.parse().ok()?))
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "仓库历史".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(HistoryPlugin::new()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随包安装读取的是 `plugin.json`，与动态库导出的身份、ABI 版本必须一致。
    #[test]
    fn manifest_matches_the_crate_identity() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["name"], "仓库历史");
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
        let (platform, entry) = if cfg!(windows) {
            ("windows", "atelyx_plugin_history.dll")
        } else {
            ("linux", "libatelyx_plugin_history.so")
        };
        assert_eq!(manifest["entry"][platform], entry);
    }
}
