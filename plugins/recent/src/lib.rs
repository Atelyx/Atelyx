//! 最近打开插件入口：面板贡献只读宿主记账的清单（`recent.list`），行点击只打开文件、
//! 不改布局（`editor.openFile` 的 `reveal: false`）；插件不记录、不剔除、不落盘。

mod model;
mod time;
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
const PLUGIN_ID: &str = "atelyx.recent";
/// 面板贡献 id（同时是面标识 `plugin:atelyx.recent:recent` 的贡献段）。
const PANEL: &str = "recent";
/// 帧拍频率：相对时间按秒刷新文案（同内容帧由控件层去重，不产生多余重绘）。
const TICK_HZ: u16 = 1;

struct RecentPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
    /// 面板面是否已挂载：未挂载时不取数也不提交帧（挂载时统一重取）。
    attached: Mutex<bool>,
}

impl RecentPlugin {
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
        match ctx.recent_list().await {
            Ok(list) => self.model.lock().unwrap().apply(list),
            Err(error) => {
                ctx.log_error(&format!("取最近打开清单失败：{error}"));
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
            ctx.log_error(&format!("提交最近打开面板失败：{error}"));
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[async_trait::async_trait]
impl PluginHandler for RecentPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "最近打开", Some("clock"), None)
            .await
        {
            ctx.log_error(&format!("注册最近打开面板失败：{error}"));
        }
        if let Err(error) = ctx.subscribe(event::name::RECENT_CHANGED).await {
            ctx.log_error(&format!("订阅 {} 失败：{error}", event::name::RECENT_CHANGED));
        }
        // 相对时间要随停留时长变化：声明低帧拍，拍里重渲染由控件层按内容去重
        self.ui.lock().await.set_fps(Some(TICK_HZ));
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
        if params.name != event::name::RECENT_CHANGED || !self.is_attached() {
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
            frame_ui.event(ctx, &params.input, |_| None).await
        };
        let mut click = None;
        let mut retry = false;
        for event in events {
            if let UiEvent::Click { id } = event {
                if id == "retry" {
                    retry = true;
                } else if let Some(target) = parse_open_id(&id) {
                    click = Some(target);
                }
            }
        }
        if retry {
            self.model.lock().unwrap().error = None;
            self.reload(ctx).await;
        }
        if let Some((gen, index)) = click {
            self.open_entry(ctx, gen, index).await;
        }
        // 尺寸与悬停 / 按下的状态都由这一帧体现：任何输入之后都要交出按当前尺寸排的帧
        self.render(ctx).await;
    }
}

impl RecentPlugin {
    /// 点开一行：世代不符（宿主还画着旧帧）的点击作废，避免按过期行号打开别的文件。
    /// 只打开文件，不新增 / 不切换视图标签：列表保持可见，布局里有对应视图时自然显示该文件。
    async fn open_entry(&self, ctx: &Context, gen: u64, index: usize) {
        let path = {
            let model = self.model.lock().unwrap();
            if model.gen != gen {
                return;
            }
            model.entries.get(index).map(|entry| entry.file.clone())
        };
        let Some(path) = path else { return };
        if let Err(error) = ctx.open_file_quiet(&path).await {
            ctx.log_error(&format!("打开最近打开条目失败：{error}"));
            self.model.lock().unwrap().fail(format!("打开失败：{error}"));
        }
    }
}

/// 解析行命中区 id（`open:<清单世代>:<行号>`）；形态不符返回 `None`。
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
            name: "最近打开".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(RecentPlugin::new()),
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
        assert_eq!(manifest["name"], "最近打开");
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
        let (platform, entry) = if cfg!(windows) {
            ("windows", "atelyx_plugin_recent.dll")
        } else {
            ("linux", "libatelyx_plugin_recent.so")
        };
        assert_eq!(manifest["entry"][platform], entry);
    }
}
