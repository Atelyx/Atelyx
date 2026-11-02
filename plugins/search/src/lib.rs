//! 搜索插件入口：面板贡献按文件名 / 路径检索当前仓库文件。清单、图标与可打开判定由宿主
//! （`vault.files`）给，过滤与排序在本插件；点击只让宿主打开文件、不改布局。

mod model;
mod view;

use std::sync::Mutex;

use atelyx_plugin_protocol::event::{self, PublishParams};
use atelyx_plugin_protocol::frame::{FrameInputParams, FrameTickParams};
use atelyx_plugin_protocol::method::{
    ContributionSlot, SurfaceAttachedParams, SurfaceDetachedParams,
};
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use crate::model::Model;

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.search";
/// 面板贡献 id（同时是面标识 `plugin:atelyx.search:search` 的贡献段）。
const PANEL: &str = "search";
/// 查询输入框的控件 id。
const INPUT: &str = "search-input";
/// 清空钮的控件 id。
const CLEAR: &str = "search-clear";
/// 取数失败空态里的重试钮 id。
const RETRY: &str = "search-retry";

struct SearchPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
    /// 面板面是否已挂载：未挂载时不取数也不提交帧（挂载时统一重取）。
    attached: Mutex<bool>,
}

impl SearchPlugin {
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

    async fn reload(&self, ctx: &Context) {
        match ctx.vault_files().await {
            Ok(list) => self.model.lock().unwrap().apply(list),
            Err(error) => {
                ctx.log_error(&format!("取仓库文件清单失败：{error}"));
                self.model.lock().unwrap().fail(error.to_string());
            }
        }
    }

    async fn render(&self, ctx: &Context) {
        if !self.is_attached() {
            return;
        }
        let snapshot = {
            let model = self.model.lock().unwrap();
            (model.view(), model.query().to_string(), model.radii.clone())
        };
        let mut ui = self.ui.lock().await;
        if let Err(error) = ui
            .render(ctx, PANEL, move |frame| {
                let (view, query, radii) = &snapshot;
                view::frame(frame, view, query, radii);
            })
            .await
        {
            ctx.log_error(&format!("提交搜索面板失败：{error}"));
        }
    }
}

/// 结果行命中区 id：带清单世代与条目下标（路径可能超出命中区 id 的字节上限）。
fn row_id(gen: u64, index: usize) -> String {
    format!("open:{gen}:{index}")
}

/// 解析行命中区 id（`open:<清单世代>:<条目下标>`）；形态不符返回 `None`。
fn parse_row_id(id: &str) -> Option<(u64, usize)> {
    let rest = id.strip_prefix("open:")?;
    let (gen, index) = rest.split_once(':')?;
    Some((gen.parse().ok()?, index.parse().ok()?))
}

#[async_trait::async_trait]
impl PluginHandler for SearchPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "搜索", Some("search"), None)
            .await
        {
            ctx.log_error(&format!("注册搜索面板失败：{error}"));
        }
        for name in [event::name::VAULT_FILES_CHANGED, event::name::VAULT_SWITCH] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
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
        self.reload(ctx).await;
        self.render(ctx).await;
    }

    async fn on_surface_detached(&self, _ctx: &Context, params: SurfaceDetachedParams) {
        if params.surface_id == PANEL {
            *self.attached.lock().unwrap() = false;
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        let watched = [event::name::VAULT_FILES_CHANGED, event::name::VAULT_SWITCH];
        if !watched.contains(&params.name.as_str()) || !self.is_attached() {
            return;
        }
        self.reload(ctx).await;
        self.render(ctx).await;
    }

    async fn on_frame_tick(&self, ctx: &Context, _params: FrameTickParams) {
        self.render(ctx).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL {
            return;
        }
        let events = {
            let mut frame_ui = self.ui.lock().await;
            frame_ui
                .event(ctx, &params.input, |id| {
                    (id == INPUT).then(|| self.model.lock().unwrap().query().to_string())
                })
                .await
        };
        let mut open = None;
        let mut cleared = false;
        let mut retry = false;
        for event in events {
            match event {
                UiEvent::Text { id, value } if id == INPUT => {
                    self.model.lock().unwrap().set_query(value);
                }
                UiEvent::Click { id } if id == CLEAR => {
                    self.model.lock().unwrap().set_query(String::new());
                    cleared = true;
                }
                UiEvent::Click { id } if id == RETRY => retry = true,
                UiEvent::Click { id } => {
                    if let Some(hit) = parse_row_id(&id) {
                        open = Some(hit);
                    }
                }
                _ => {}
            }
        }
        if cleared {
            self.ui.lock().await.focus_input(INPUT, "");
        }
        if retry {
            self.model.lock().unwrap().clear_error();
            self.reload(ctx).await;
        }
        if let Some((gen, index)) = open {
            self.open_row(ctx, gen, index).await;
        }
        // 尺寸与悬停 / 按下的状态都由这一帧体现：任何输入之后都要交出按当前尺寸排的帧
        self.render(ctx).await;
    }
}

impl SearchPlugin {
    /// 点开一行：世代不符（宿主还画着旧帧）的点击作废，避免按过期行号打开别的文件。
    async fn open_row(&self, ctx: &Context, gen: u64, index: usize) {
        let path = {
            let model = self.model.lock().unwrap();
            if model.gen() != gen {
                return;
            }
            model.entry(index).map(|entry| entry.path.clone())
        };
        let Some(path) = path else { return };
        if let Err(error) = ctx.open_file(&path).await {
            ctx.log_error(&format!("打开搜索结果失败：{error}"));
        }
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "搜索".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(SearchPlugin::new()),
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
        assert_eq!(manifest["name"], "搜索");
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
        let (platform, entry) = if cfg!(windows) {
            ("windows", "atelyx_plugin_search.dll")
        } else {
            ("linux", "libatelyx_plugin_search.so")
        };
        assert_eq!(manifest["entry"][platform], entry);
    }

    #[test]
    fn row_ids_round_trip() {
        assert_eq!(parse_row_id(&row_id(3, 7)), Some((3, 7)));
        assert_eq!(parse_row_id("mode:全局搜索"), None);
        assert_eq!(parse_row_id("open:abc"), None);
        assert_eq!(parse_row_id("open:3:abc"), None);
    }
}
