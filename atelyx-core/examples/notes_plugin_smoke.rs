//! 笔记预置插件的无界面冒烟：装载、挂面、打开文档、三档切换与块级编辑走一遍。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, PluginRuntime, PluginStatus, SurfaceKind,
};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};

const PLUGIN: &str = "atelyx.notes";
const SURFACE: &str = "note";
const NOTE: &str = "# 标题\n\n第一段正文。\n\n- 列表项一\n- 列表项二\n";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = std::env::temp_dir().join(format!("atelyx-notes-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.clone(), "0.6.5");
    runtime.scan();

    let responder = runtime.clone();
    std::thread::spawn(move || {
        let responder_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应答线程运行时");
        responder_runtime.block_on(async move {
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
                                width: item.text.chars().map(|ch| item.size * if (ch as u32) < 0x1100 { 0.55 } else { 1.0 }).sum(),
                                height: item.size * 1.2,
                                lines: 1,
                            })
                            .collect();
                        reply(&responder, request_id, json!({ "sizes": sizes }));
                    }
                    HostRequest::NoteOpen { request_id, params, .. } => {
                        reply(
                            &responder,
                            request_id,
                            json!({ "path": params.path, "text": NOTE, "canWrite": true }),
                        );
                    }
                    HostRequest::NoteBacklinks { request_id, .. } => {
                        reply(&responder, request_id, json!({ "entries": [] }));
                    }
                    HostRequest::NoteEdit { request_id, .. }
                    | HostRequest::NoteAwareness { request_id, .. } => {
                        reply(&responder, request_id, json!({}));
                    }
                    HostRequest::NoteSave { request_id, .. } => {
                        reply(&responder, request_id, json!({}));
                    }
                    HostRequest::HistoryOpen { request_id, .. } => {
                        reply(&responder, request_id, json!({}));
                    }
                    other => eprintln!("未处理的宿主请求：{other:?}"),
                }
            }
        });
    });

    let enable = runtime.clone();
    let plugin = PLUGIN.to_string();
    tokio::task::spawn_blocking(move || enable.set_enabled(&plugin, true))
        .await
        .expect("启用任务不应 panic")
        .expect("启用笔记插件");
    wait_status(&runtime, PLUGIN).await;

    runtime.notify_surface_attached(PLUGIN, SURFACE, SurfaceKind::Panel);
    runtime.notify_frame_input(
        PLUGIN,
        SURFACE,
        &FrameInput::Resize { width: 900.0, height: 700.0 },
    );
    let frame = wait_frame(&runtime, Duration::from_secs(5)).await.expect("挂面后应有帧");
    println!("面板首帧：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());

    runtime.notify_doc_open(PLUGIN, SURFACE, "a.md");
    let frame = wait_frame(&runtime, Duration::from_secs(5)).await.expect("打开笔记后应有帧");
    println!("只读帧：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());

    if let Some(hit) = frame.hits.iter().find(|hit| hit.id == "note:mode:edit").cloned() {
        click_hit(&runtime, &hit);
        let frame = wait_hit(&runtime, "note:block:0", Duration::from_secs(2))
            .await
            .expect("切换预览编辑档后应有块命中");
        let blocks = frame.hits.iter().filter(|hit| hit.id.starts_with("note:block:")).count();
        println!("预览编辑块：{blocks}");
    }
    if let Some(hit) = runtime
        .surface_frame(PLUGIN, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "note:block:0").cloned())
    {
        double_click(&runtime, &hit);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let focused = runtime.surface_frame(PLUGIN, SURFACE).is_some_and(|frame| {
            frame.hits.iter().any(|hit| hit.id == "note:block:0")
                && frame
                    .commands
                    .iter()
                    .any(|command| matches!(command, atelyx_plugin_protocol::paint::DrawCommand::Text { content, .. } if content == "# 标题"))
        });
        println!("双击揭示块源码：{}", if focused { "通过" } else { "未确认" });
        runtime.notify_frame_input(
            PLUGIN,
            SURFACE,
            &FrameInput::Text {
                text: "改".into(),
                composing: false,
                caret: None,
                candidates: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
        println!("块内输入后帧：{}", if runtime.surface_frame(PLUGIN, SURFACE).is_some() { "有" } else { "缺失" });
    }
    if let Some(hit) = runtime
        .surface_frame(PLUGIN, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "note:mode:source").cloned())
    {
        click_hit(&runtime, &hit);
        let frame = wait_hit(&runtime, "note:source", Duration::from_secs(2))
            .await
            .expect("源码档应有输入面");
        println!("源码档：{} 条指令", frame.commands.len());
    }
    runtime.notify_surface_detached(PLUGIN, SURFACE);
    runtime.set_enabled(PLUGIN, false).expect("停用笔记插件");
    println!("停用：通过");
    if let Ok(log) = std::fs::read_to_string(root.join(PLUGIN).join("logs").join("runtime.log")) {
        println!("插件日志：\n{log}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn reply(runtime: &Arc<PluginRuntime>, request_id: u64, value: Value) {
    runtime.resolve_shell_call(request_id, Ok(value));
}

fn click_hit(runtime: &Arc<PluginRuntime>, hit: &atelyx_plugin_protocol::frame::HitRegion) {
    let x = hit.x + hit.w / 2.0;
    let y = hit.y + hit.h / 2.0;
    for input in [
        FrameInput::PointerDown {
            x,
            y,
            button: atelyx_core::plugin_runtime::MouseButton::Left,
            hit: Some(hit.id.clone()),
        },
        FrameInput::PointerUp {
            x,
            y,
            button: atelyx_core::plugin_runtime::MouseButton::Left,
            hit: Some(hit.id.clone()),
        },
    ] {
        runtime.notify_frame_input(PLUGIN, SURFACE, &input);
    }
}

fn double_click(runtime: &Arc<PluginRuntime>, hit: &atelyx_plugin_protocol::frame::HitRegion) {
    let x = hit.x + hit.w / 2.0;
    let y = hit.y + hit.h / 2.0;
    runtime.notify_frame_input(
        PLUGIN,
        SURFACE,
        &FrameInput::DoubleClick {
            x,
            y,
            button: atelyx_core::plugin_runtime::MouseButton::Left,
            hit: Some(hit.id.clone()),
        },
    );
}

async fn wait_hit(
    runtime: &Arc<PluginRuntime>,
    hit_id: &str,
    timeout: Duration,
) -> Option<Arc<SurfaceFrame>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(frame) = runtime.surface_frame(PLUGIN, SURFACE) {
            if frame.hits.iter().any(|hit| hit.id == hit_id) {
                return Some(frame);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_frame(runtime: &Arc<PluginRuntime>, timeout: Duration) -> Option<Arc<SurfaceFrame>> {
    let deadline = Instant::now() + timeout;
    let mut last: Option<Arc<SurfaceFrame>> = None;
    loop {
        if let Some(frame) = runtime.surface_frame(PLUGIN, SURFACE) {
            if !frame.hits.is_empty() {
                return Some(frame);
            }
            last = Some(frame);
        }
        if Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_status(runtime: &Arc<PluginRuntime>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = runtime
            .list()
            .into_iter()
            .find(|plugin| plugin.id == id)
            .map(|plugin| plugin.status)
            .unwrap_or(PluginStatus::Stopped);
        if status == PluginStatus::Running {
            return;
        }
        if Instant::now() >= deadline {
            panic!("插件未进入运行态：{status:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn copy_tree(source: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

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
        font_sizes: FontSizes {
            micro: 11.0,
            caption: 12.0,
            ui: 13.0,
            body: 15.0,
            h2: 18.0,
            h1: 22.0,
        },
        radii: Radii { xs: 3.0, sm: 5.0, md: 8.0 },
        event_colors: vec![],
    }
}
