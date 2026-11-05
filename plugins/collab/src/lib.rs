//! 协作房间插件入口：面板贡献展示当前空间的连接状态、成员名册与在线设备。
//! 容器、连接态、名册与在线设备都由宿主给（`space.room` / `space.members`），
//! 「协作设置」与「协作空间」入口是宿主的弹层 / 菜单，插件只发出请求。

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

use crate::model::{Model, Palette};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.collab";
/// 面板贡献 id（同时是面标识 `plugin:atelyx.collab:collabroom` 的贡献段）。
const PANEL: &str = "collabroom";
/// 「协作设置」入口的命中区 id（与视图里的声明同值）。
const SETTINGS: &str = "space-settings";
/// 空态里的「协作空间」入口命中区 id（与视图里的声明同值）。
const OPEN_MENU: &str = "space-open-menu";
/// 房间快照读取失败空态里的重试钮 id。
const RETRY: &str = "room-retry";

struct CollabPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
    /// 面板面是否已挂载：未挂载时不取数也不提交帧（挂载时统一重取）。
    attached: Mutex<bool>,
}

impl CollabPlugin {
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
            Ok(theme) => {
                let mut model = self.model.lock().unwrap();
                model.palette = Palette {
                    accent: theme.colors.accent.clone(),
                    text_secondary: theme.colors.text_secondary.clone(),
                    text_muted: theme.colors.text_muted.clone(),
                    radii: theme.radii,
                };
            }
            Err(error) => ctx.log_error(&format!("取宿主主题失败，按缺省色绘制：{error}")),
        }
    }

    async fn reload_room(&self, ctx: &Context) {
        match ctx.space_room().await {
            Ok(room) => self.model.lock().unwrap().apply_room(room),
            Err(error) => {
                ctx.log_error(&format!("取协作房间快照失败：{error}"));
                self.model.lock().unwrap().fail_room(error.to_string());
            }
        }
    }

    async fn reload_members(&self, ctx: &Context) {
        match ctx.space_members().await {
            Ok(members) => self.model.lock().unwrap().apply_members(members),
            Err(error) => {
                ctx.log_error(&format!("取空间成员名册失败：{error}"));
                self.model.lock().unwrap().fail_members(error.to_string());
            }
        }
    }

    async fn reload(&self, ctx: &Context) {
        self.reload_room(ctx).await;
        self.reload_members(ctx).await;
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
            ctx.log_error(&format!("提交协作房间面板失败：{error}"));
        }
    }
}

/// 打开文件行的命中区 id 解析结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileHit {
    gen: u64,
    device: usize,
    file: usize,
}

/// 解析 `file:<快照世代>:<设备行>:<文件行>`；形态不符返回 `None`。
fn parse_file_id(id: &str) -> Option<FileHit> {
    let rest = id.strip_prefix("file:")?;
    let parts: Vec<&str> = rest.split(':').collect();
    let [gen, device, file] = parts.as_slice() else { return None };
    Some(FileHit { gen: gen.parse().ok()?, device: device.parse().ok()?, file: file.parse().ok()? })
}

#[async_trait::async_trait]
impl PluginHandler for CollabPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "协作房间", Some("users"), None)
            .await
        {
            ctx.log_error(&format!("注册协作房间面板失败：{error}"));
        }
        for name in [event::name::SPACE_PEERS_CHANGED, event::name::SPACE_MEMBERS_CHANGED] {
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
        // 先交一帧：首份快照未到时是加载态，重挂时是已有房间内容
        self.render(ctx).await;
        self.reload(ctx).await;
        self.render(ctx).await;
    }

    async fn on_surface_detached(&self, _ctx: &Context, params: SurfaceDetachedParams) {
        if params.surface_id == PANEL {
            *self.attached.lock().unwrap() = false;
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        if !self.is_attached() {
            return;
        }
        match params.name.as_str() {
            event::name::SPACE_PEERS_CHANGED => self.reload_room(ctx).await,
            event::name::SPACE_MEMBERS_CHANGED => self.reload_members(ctx).await,
            _ => return,
        }
        self.render(ctx).await;
    }

    async fn on_frame_tick(&self, ctx: &Context, _params: FrameTickParams) {
        self.render(ctx).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL {
            return;
        }
        let events = self.ui.lock().await.event(ctx, &params.input, |_| None).await;
        let mut retry = false;
        for event in events {
            let UiEvent::Click { id } = event else { continue };
            if id == RETRY {
                retry = true;
            } else if id == SETTINGS {
                if let Err(error) = ctx.open_space_settings().await {
                    ctx.log_error(&format!("打开协作设置失败：{error}"));
                }
            } else if id == OPEN_MENU {
                if let Err(error) = ctx.open_space_menu().await {
                    ctx.log_error(&format!("打开协作空间菜单失败：{error}"));
                }
            } else if let Some(hit) = parse_file_id(&id) {
                self.open_file(ctx, hit).await;
            }
        }
        if retry {
            self.model.lock().unwrap().clear_room_error();
            self.reload_room(ctx).await;
        }
        // 尺寸与悬停 / 按下的状态都由这一帧体现：任何输入之后都要交出按当前尺寸排的帧
        self.render(ctx).await;
    }
}

impl CollabPlugin {
    /// 点开别人打开的文件：世代不符（宿主还画着旧帧）的点击作废。
    async fn open_file(&self, ctx: &Context, hit: FileHit) {
        let path = {
            let model = self.model.lock().unwrap();
            if model.gen != hit.gen {
                return;
            }
            model
                .devices()
                .get(hit.device)
                .and_then(|device| device.files.get(hit.file))
                .map(|file| file.path.clone())
        };
        let Some(path) = path else { return };
        if let Err(error) = ctx.open_file(&path).await {
            ctx.log_error(&format!("打开协作设备上的文件失败：{error}"));
        }
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "协作房间".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(CollabPlugin::new()),
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
        assert_eq!(manifest["name"], "协作房间");
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["abiVersion"], atelyx_plugin_sdk::abi::ABI_VERSION);
        let (platform, entry) = if cfg!(windows) {
            ("windows", "atelyx_plugin_collab.dll")
        } else {
            ("linux", "libatelyx_plugin_collab.so")
        };
        assert_eq!(manifest["entry"][platform], entry);
    }

    #[test]
    fn file_ids_round_trip() {
        assert_eq!(parse_file_id("file:3:1:2"), Some(FileHit { gen: 3, device: 1, file: 2 }));
        assert_eq!(parse_file_id("file:3:1"), None);
        assert_eq!(parse_file_id("space-settings"), None);
        assert_eq!(parse_file_id("file:a:1:2"), None);
    }
}
