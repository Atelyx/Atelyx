//! 同进程日历插件的无界面冒烟：经真实 `PluginRuntime` 加载、挂面、收帧、点击、停用。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, MouseButton, PluginRuntime, PluginStatus,
    SurfaceKind,
};
use atelyx_plugin_protocol::event::VaultSwitchPayload;
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};

const ID: &str = "com.atelyx.calendar";
const SURFACE: &str = "calendar";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    // 在临时副本上跑：卸载会真删插件目录（也验证 Windows 上动态库能随实例释放）
    let root = std::env::temp_dir().join(format!("atelyx-abi-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.clone(), "0.6.5");
    runtime.scan();

    let responder = runtime.clone();
    tokio::spawn(async move {
        while let Some(request) = responder.recv_host_request().await {
            match request {
                HostRequest::QueryTheme { request_id, .. } => {
                    reply(&responder, request_id, serde_json::to_value(theme()).unwrap());
                }
                HostRequest::MeasureBatch { request_id, params, .. } => {
                    let sizes: Vec<MeasureTextResult> = params
                        .items
                        .iter()
                        .map(|item| MeasureTextResult {
                            width: item.text.chars().count() as f32 * item.size * 0.55,
                            height: item.size * 1.2,
                            lines: 1,
                        })
                        .collect();
                    reply(&responder, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::HistoryActivity { request_id, .. } => {
                    reply(&responder, request_id, json!({ "days": [] }));
                }
                HostRequest::VaultExists { request_id, .. } => {
                    reply(&responder, request_id, json!({ "exists": false }));
                }
                HostRequest::VaultReadText { request_id, .. } => {
                    responder.resolve_shell_call(request_id, Err("冒烟不读文件".into()));
                }
                other => eprintln!("未处理的宿主请求：{other:?}"),
            }
        }
    });

    let enabling = {
        let runtime = runtime.clone();
        tokio::task::spawn_blocking(move || runtime.set_enabled(ID, true))
    };
    enabling
        .await
        .expect("启用任务不应 panic")
        .expect("启用日历插件");
    wait_running(&runtime).await;
    wait_service(&runtime).await;
    println!("插件列表：{:?}", runtime.list());

    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "smoke".into() });
    runtime.notify_surface_attached(ID, SURFACE, SurfaceKind::Panel);
    let frame = wait_frame(&runtime, Duration::from_secs(3)).await.expect("attach 后应有帧");
    assert!(!frame.hits.is_empty(), "日历帧应有命中区");
    println!("首帧：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());

    let hit = frame
        .hits
        .iter()
        .find(|hit| hit.id == "next-month")
        .expect("应有翻月按钮");
    let hit_id = hit.id.clone();
    let x = hit.x + hit.w / 2.0;
    let y = hit.y + hit.h / 2.0;
    let before = frame;
    runtime.notify_frame_input(
        ID,
        SURFACE,
        &FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: Some(hit_id.clone()) },
    );
    runtime.notify_frame_input(
        ID,
        SURFACE,
        &FrameInput::PointerUp { x, y, button: MouseButton::Left, hit: Some(hit_id) },
    );
    let frame = wait_frame_after(&runtime, &before, Duration::from_secs(3))
        .await
        .expect("点击后应有新帧");
    println!("点击后：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());

    toggle(&runtime, false).await.expect("停用");
    toggle(&runtime, true).await.expect("重新启用");
    wait_service(&runtime).await;
    runtime.notify_surface_attached(ID, SURFACE, SurfaceKind::Panel);
    let frame = wait_frame(&runtime, Duration::from_secs(3)).await.expect("重新启用后应有帧");
    assert!(!frame.hits.is_empty(), "重新启用后应恢复交互");
    println!("重新启用：通过");

    let installed = {
        let runtime = runtime.clone();
        let source = source.join(ID);
        tokio::task::spawn_blocking(move || runtime.install_dir(&source))
            .await
            .expect("更新任务不应 panic")
            .expect("从来源更新应成功")
    };
    assert!(installed.start_error.is_none(), "{:?}", installed.start_error);
    wait_service(&runtime).await;
    runtime.notify_surface_attached(ID, SURFACE, SurfaceKind::Panel);
    let frame = wait_frame(&runtime, Duration::from_secs(3)).await.expect("更新后应有帧");
    assert!(!frame.hits.is_empty(), "更新后应恢复交互");
    println!("更新：通过");

    runtime.shutdown();
    let status = runtime.list().into_iter().find(|plugin| plugin.id == ID).map(|plugin| plugin.status);
    assert_eq!(status, Some(PluginStatus::Stopped), "停用后应回到 Stopped");
    println!("停用：通过");

    runtime.uninstall(ID).expect("卸载动态库插件应成功");
    assert!(!root.join(ID).exists(), "卸载后插件目录应已删除");
    let _ = std::fs::remove_dir_all(&root);
    println!("卸载：通过");
}

async fn toggle(runtime: &Arc<PluginRuntime>, on: bool) -> Result<(), String> {
    let runtime = runtime.clone();
    tokio::task::spawn_blocking(move || runtime.set_enabled(ID, on))
        .await
        .expect("启停任务不应 panic")
}

/// 冒烟用的目录复制（保留目录层级）。
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn reply(runtime: &PluginRuntime, request_id: u64, result: Value) {
    runtime.resolve_shell_call(request_id, Ok(result));
}

/// 宿主 `query.theme` 的正常应答：字号 / 圆角必须为正，否则插件画出的文字尺寸为 0。
fn theme() -> ThemeResult {
    ThemeResult {
        colors: ThemeColors {
            text: "#1f2328".into(),
            text_secondary: "#57606a".into(),
            text_muted: "#8c959f".into(),
            accent: "#4a90d9".into(),
            accent_soft: "#dbeafe".into(),
            danger: "#d1242f".into(),
            danger_soft: "#ffebe9".into(),
            success: "#1a7f37".into(),
            success_soft: "#dafbe1".into(),
            warning: "#9a6700".into(),
            warning_soft: "#fff8c5".into(),
            surface: "#ffffff".into(),
            surface_muted: "#f6f8fa".into(),
            surface_sunken: "#eaeef2".into(),
            hover: "#f0f3f6".into(),
            border: "#d0d7de".into(),
            border_strong: "#8c959f".into(),
            transparent: "#00000000".into(),
        },
        font_sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
        radii: Radii { xs: 4.0, sm: 6.0, md: 8.0 },
        event_colors: vec!["#e06c75".into(), "#61afef".into(), "#98c379".into()],
    }
}

async fn wait_running(runtime: &PluginRuntime) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = runtime.list().into_iter().find(|plugin| plugin.id == ID).map(|plugin| plugin.status);
        if matches!(status, Some(PluginStatus::Running)) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("插件未进入运行态：{status:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 启用是异步的：等到插件在服务目录里登记成功再挂面，按真实启动时序冒烟。
async fn wait_service(runtime: &PluginRuntime) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ready = runtime
            .service_list()
            .services
            .iter()
            .any(|service| service.service == "calendar" && service.provider.is_some());
        if ready {
            return;
        }
        if Instant::now() >= deadline {
            panic!("日历服务未登记成功");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_frame(runtime: &PluginRuntime, timeout: Duration) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(frame) = runtime.surface_frame(ID, SURFACE) {
            return Some((*frame).clone());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_frame_after(
    runtime: &PluginRuntime,
    seen: &SurfaceFrame,
    timeout: Duration,
) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(frame) = runtime.surface_frame(ID, SURFACE) {
            if frame.as_ref() != seen {
                return Some((*frame).clone());
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
