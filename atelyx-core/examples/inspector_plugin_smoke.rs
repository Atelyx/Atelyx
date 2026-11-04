//! 属性面板插件无界面冒烟：经真实 `PluginRuntime` 加载、挂面、取帧，覆盖笔记属性渲染、
//! 新增属性经宿主写回、画布前台切到画布属性与停用收尾。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, MouseButton, PluginRuntime, PluginStatus,
    SurfaceKind, VaultFileEntry, FilesResult,
};
use atelyx_plugin_protocol::event::DocChangedPayload;
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};
use atelyx_plugin_protocol::paint::{DrawCommand, TextAlign, TextLayout};

const INSPECTOR_ID: &str = "atelyx.inspector";
const CANVAS_ID: &str = "atelyx.canvas";
const SURFACE: &str = "inspector";
const PANEL_W: f32 = 320.0;
const PANEL_H: f32 = 400.0;
/// 空态图标边长与内容块行距（与插件占位组合一致）。
const EMPTY_ICON: f32 = 64.0;
const EMPTY_GAP: f32 = 16.0;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = atelyx_core::scratch::TempDir::new("inspector-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
    runtime.scan();

    let vault = atelyx_core::scratch::TempDir::new("inspector-vault");
    std::fs::write(vault.join("demo.atlx"), canvas_json()).expect("写初始画布");

    let state = Arc::new(HostState::default());
    serve(runtime.clone(), state.clone(), vault.to_path_buf());

    if !running(&runtime, INSPECTOR_ID) {
        enable(&runtime, INSPECTOR_ID).await;
    }
    wait_running(&runtime, INSPECTOR_ID).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    runtime.notify_frame_input(INSPECTOR_ID, SURFACE, &FrameInput::Resize { width: PANEL_W, height: PANEL_H });
    runtime.notify_surface_attached(INSPECTOR_ID, SURFACE, SurfaceKind::Panel);
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if runtime.surface_frame(INSPECTOR_ID, SURFACE).is_some_and(|f| f.size.is_some()) {
            break;
        }
    }

    // 无属性：空态整块在可用区域内水平垂直居中
    let empty = wait_frame_text(&runtime, "当前笔记无属性。").await.unwrap_or_else(|| panic!("无属性时应出空态"));
    assert_empty_state_centered(&empty);

    // 切到有属性：宿主广播 doc:changed 后插件重读属性区
    *state.present.lock().unwrap() = true;
    runtime.on_doc_changed(DocChangedPayload {
        path: "notes/demo.md".into(),
        text: None,
        dirty: false,
        save_error: None,
    });

    // 笔记属性：键名与值都画进帧
    let frame = wait_frame(&runtime, |frame| has_text(frame, "标题")).await.unwrap_or_else(|| {
        let texts: Vec<String> = runtime
            .surface_frame(INSPECTOR_ID, SURFACE)
            .map(|frame| {
                frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text { content, .. } => Some(content.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        panic!("应出属性帧，当前文本：{texts:?}");
    });
    assert!(has_text(&frame, "正文值"), "属性值应渲染");
    println!("载入：{} 条指令 / {} 个命中区", frame.commands.len(), frame.hits.len());

    // 点「添加属性」：插件把占位键写回宿主
    let add = frame
        .hits
        .iter()
        .find(|hit| hit.id == "props:add")
        .expect("应有添加属性入口");
    let (x, y) = (add.x + add.w / 2.0, add.y + add.h / 2.0);
    runtime.notify_frame_input(INSPECTOR_ID, SURFACE, &FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: Some("props:add".into()) });
    runtime.notify_frame_input(INSPECTOR_ID, SURFACE, &FrameInput::PointerUp { x, y, button: MouseButton::Left, hit: Some("props:add".into()) });
    let block = wait_written(&state, 1).await;
    assert!(block.contains("property:"), "写回块应含占位属性：{block}");

    // 画布在前台：属性面切到画布统计
    if !running(&runtime, CANVAS_ID) {
        enable(&runtime, CANVAS_ID).await;
    }
    wait_running(&runtime, CANVAS_ID).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    runtime.notify_surface_attached(CANVAS_ID, "canvas", SurfaceKind::Panel);
    runtime.on_canvas_visible(true);
    for _ in 0..20 {
        runtime.notify_frame_input(CANVAS_ID, "canvas", &FrameInput::Resize { width: 1200.0, height: 800.0 });
        runtime.on_open_files_changed();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if running(&runtime, CANVAS_ID)
            && runtime.surface_frame(CANVAS_ID, "canvas").is_some_and(|frame| !frame.commands.is_empty())
        {
            break;
        }
    }
    let frame = wait_frame_text(&runtime, "选中一张卡查看其属性").await.unwrap_or_else(|| {
        let texts: Vec<String> = runtime
            .surface_frame(INSPECTOR_ID, SURFACE)
            .map(|frame| {
                frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text { content, .. } => Some(content.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        panic!("画布前台应显示画布属性，当前文本：{texts:?}");
    });
    assert!(has_text(&frame, "卡") && has_text(&frame, "连线"), "应显示画布统计行");

    disable(&runtime, INSPECTOR_ID).await;
    let status = runtime.list().into_iter().find(|p| p.id == INSPECTOR_ID).map(|p| p.status);
    assert!(matches!(status, Some(PluginStatus::Stopped) | None), "停用后应退出运行态");
    println!("停用：通过");
    runtime.shutdown();
}

fn canvas_json() -> String {
    json!({
        "schema": "atelyx-canvas/v1",
        "id": "cv-smoke",
        "title": "demo",
        "nodes": [],
        "edges": [],
        "createdAt": 0,
        "updatedAt": 0
    })
    .to_string()
}

#[derive(Default)]
struct HostState {
    written: Mutex<Vec<String>>,
    vault: Mutex<PathBuf>,
    unhandled: Mutex<Vec<String>>,
    /// 当前笔记是否有 frontmatter 块（false = 属性面板空态）。
    present: Mutex<bool>,
}

fn serve(runtime: Arc<PluginRuntime>, state: Arc<HostState>, vault: PathBuf) {
    *state.vault.lock().unwrap() = vault;
    tokio::spawn(async move {
        while let Some(request) = runtime.recv_host_request().await {
            match request {
                HostRequest::MeasureBatch { request_id, params, .. } => {
                    let sizes: Vec<MeasureTextResult> = params.items.iter().map(measure).collect();
                    reply(&runtime, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::QueryTheme { request_id, .. } => {
                    reply(&runtime, request_id, serde_json::to_value(theme()).unwrap());
                }
                HostRequest::QueryFonts { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "families": ["Sans Serif"] }));
                }
                HostRequest::VaultTags { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "tags": ["a", "b"] }));
                }
                HostRequest::DocPropsRead { request_id, .. } => {
                    let present = *state.present.lock().unwrap();
                    let block = if present {
                        state.written.lock().unwrap().last().cloned().unwrap_or_else(initial_block)
                    } else {
                        String::new()
                    };
                    reply(
                        &runtime,
                        request_id,
                        json!({
                            "path": "notes/demo.md",
                            "frontmatter": block,
                            "present": present,
                            "parsed": present,
                            "editable": true
                        }),
                    );
                }
                HostRequest::DocPropsWrite { request_id, params, .. } => {
                    state.written.lock().unwrap().push(params.frontmatter);
                    reply(&runtime, request_id, json!({}));
                }
                HostRequest::EditorOpenFiles { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "paths": ["demo.atlx"] }));
                }
                HostRequest::AiAgents { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "agents": [{ "id": "chat", "name": "对话", "builtin": true }] }));
                }
                HostRequest::AiModels { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "providers": [] }));
                }
                HostRequest::PanelHeader { request_id, .. } => {
                    reply(&runtime, request_id, json!({}));
                }
                HostRequest::VaultListDirs { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "dirs": [] }));
                }
                HostRequest::CollabPeers { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "connected": false }));
                }
                HostRequest::VaultReadText { request_id, path, .. } => {
                    let target = state.vault.lock().unwrap().join(path);
                    match std::fs::read_to_string(&target) {
                        Ok(content) => reply(&runtime, request_id, json!({ "content": content })),
                        Err(error) => runtime.resolve_shell_call(request_id, Err(error.to_string())),
                    }
                }
                HostRequest::SpaceRoom { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "kind": "local", "name": "本地" }));
                }
                HostRequest::VaultFiles { request_id, .. } => {
                    reply(
                        &runtime,
                        request_id,
                        serde_json::to_value(FilesResult {
                            files: vec![VaultFileEntry {
                                path: "demo.atlx".into(),
                                name: "demo.atlx".into(),
                                icon: "shapes".into(),
                                openable: true,
                            }],
                        })
                        .unwrap(),
                    );
                }
                other => state.unhandled.lock().unwrap().push(format!("{other:?}")),
            }
        }
    });
}

fn initial_block() -> String {
    "---\n标题: 正文值\n---\n".to_string()
}

fn measure(item: &atelyx_plugin_protocol::method::MeasureItem) -> MeasureTextResult {
    let layout = item.layout.unwrap_or(TextLayout { line_height: None, wrap: None, shaping: None });
    let line_height = item.size * layout.line_height.unwrap_or(1.2);
    let wrap_w = item.max_width.unwrap_or(f32::MAX);
    let mut segments = Vec::new();
    for segment in item.text.split('\n') {
        let mut current = String::new();
        let mut width = 0.0f32;
        for ch in segment.chars() {
            let advance = char_advance(ch, item.size);
            if !current.is_empty() && width + advance > wrap_w {
                segments.push(atelyx_plugin_protocol::method::MeasureSegment { text: std::mem::take(&mut current), width });
                width = 0.0;
            }
            current.push(ch);
            width += advance;
        }
        segments.push(atelyx_plugin_protocol::method::MeasureSegment { text: current, width });
    }
    let width = segments.iter().map(|seg| seg.width).fold(0.0, f32::max);
    MeasureTextResult { width, height: line_height * segments.len().max(1) as f32, lines: segments.len().max(1) as u32, segments: Some(segments) }
}

fn theme() -> ThemeResult {
    ThemeResult {
        colors: ThemeColors {
            text: "#1f2328".into(),
            text_secondary: "#57606a".into(),
            text_muted: "#8c959f".into(),
            accent: "#e0a94e".into(),
            accent_soft: "#e0a94e24".into(),
            danger: "#e8776c".into(),
            danger_soft: "#e8776c24".into(),
            success: "#5fbf8b".into(),
            success_soft: "#5fbf8b24".into(),
            warning: "#efc75e".into(),
            warning_soft: "#efc75e24".into(),
            surface: "#1b1e23".into(),
            surface_muted: "#131418".into(),
            surface_sunken: "#0c0d10".into(),
            hover: "#23272e".into(),
            border: "#2e333a".into(),
            border_strong: "#383d44".into(),
            background: "#131418".into(),
            border_subtle: "#252a31".into(),
            selection: "#e0a94e40".into(),
            scrim: "#00000099".into(),
            transparent: "#00000000".into(),
        },
        font_sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
        radii: Radii { xs: 4.0, sm: 6.0, md: 10.0 },
        event_colors: Vec::new(),
    }
}

fn running(runtime: &PluginRuntime, id: &str) -> bool {
    runtime.list().into_iter().any(|p| p.id == id && p.status == PluginStatus::Running)
}

async fn enable(runtime: &Arc<PluginRuntime>, id: &str) {
    let runtime = runtime.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || runtime.set_enabled(&id, true))
        .await
        .expect("启用任务不应 panic")
        .expect("启用预置插件");
}

async fn disable(runtime: &Arc<PluginRuntime>, id: &str) {
    let runtime = runtime.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || runtime.set_enabled(&id, false))
        .await
        .expect("停用任务不应 panic")
        .expect("停用预置插件");
}

async fn wait_running(runtime: &PluginRuntime, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !running(runtime, id) {
        assert!(Instant::now() < deadline, "插件未进入运行态：{id}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn reply(runtime: &PluginRuntime, request_id: u64, result: Value) {
    runtime.resolve_shell_call(request_id, Ok(result));
}

fn has_text(frame: &SurfaceFrame, wanted: &str) -> bool {
    frame.commands.iter().any(|command| matches!(command, DrawCommand::Text { content, .. } if content == wanted))
}

/// 空态标题与说明的对齐盒中心落在面宽中心，内容块（图标 + 标题 + 说明）的垂直中心落在面高中心。
fn assert_empty_state_centered(frame: &SurfaceFrame) {
    let title = text_geometry(frame, "笔记属性").expect("空态应有标题");
    let desc = text_geometry(frame, "当前笔记无属性。").expect("空态应有说明");
    for (label, text) in [("标题", &title), ("说明", &desc)] {
        let width = text.max_width.expect("空态文字应按面宽对齐");
        assert_eq!(text.align, Some(TextAlign::Center), "空态{label}应居中");
        assert!(
            (text.x + width / 2.0 - PANEL_W / 2.0).abs() < 0.5,
            "空态{label}应水平居中：x={} 宽={width}",
            text.x
        );
    }
    let group_h = EMPTY_ICON + EMPTY_GAP + title.size * 1.2 + EMPTY_GAP + desc.size * 1.2;
    let group_top = (PANEL_H - group_h) / 2.0;
    let title_y = group_top + EMPTY_ICON + EMPTY_GAP;
    let desc_y = title_y + title.size * 1.2 + EMPTY_GAP;
    assert!(
        (title.y - title_y).abs() < 0.5,
        "空态标题应垂直居中：y={} 期望={title_y}",
        title.y
    );
    assert!(
        (desc.y - desc_y).abs() < 0.5,
        "空态说明应垂直居中：y={} 期望={desc_y}",
        desc.y
    );
}

struct TextGeom {
    x: f32,
    y: f32,
    size: f32,
    max_width: Option<f32>,
    align: Option<TextAlign>,
}

fn text_geometry(frame: &SurfaceFrame, wanted: &str) -> Option<TextGeom> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { x, y, size, content, max_width, align, .. } if content == wanted => {
            Some(TextGeom { x: *x, y: *y, size: *size, max_width: *max_width, align: *align })
        }
        _ => None,
    })
}

fn char_advance(ch: char, size: f32) -> f32 {
    size * if ch.is_ascii() { 0.55 } else { 1.0 }
}

async fn wait_frame(runtime: &PluginRuntime, accept: impl Fn(&SurfaceFrame) -> bool) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(frame) = runtime.surface_frame(INSPECTOR_ID, SURFACE) {
            if accept(&frame) {
                return Some((*frame).clone());
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_frame_text(runtime: &PluginRuntime, text: &str) -> Option<SurfaceFrame> {
    wait_frame(runtime, |frame| has_text(frame, text)).await
}

async fn wait_written(state: &HostState, count: usize) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.written.lock().unwrap().len() < count {
        assert!(
            Instant::now() < deadline,
            "宿主未收到属性写回；未接线的宿主请求：{:?}",
            state.unhandled.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    state.written.lock().unwrap().last().cloned().unwrap_or_default()
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
