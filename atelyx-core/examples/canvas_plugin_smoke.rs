//! 画布插件无界面冒烟：经真实 `PluginRuntime` 加载、挂面、取帧，覆盖载入渲染、右键新建卡、
//! 卡片拖动与防抖落盘、撤销、视口（滚轮方向 / 字号固定 / 占位阈值 / 改尺寸保持视口）、
//! 面板头部脏标记与停用收尾。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FilesResult, FrameInput, HostRequest, MeasureTextResult, Modifiers, MouseButton,
    PanelHeaderParams, PluginRuntime, PluginStatus, SurfaceKind, VaultFileEntry,
};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};
use atelyx_plugin_protocol::paint::{DrawCommand, TextAlign, TextLayout};

const CANVAS_ID: &str = "atelyx.canvas";
const SURFACE: &str = "canvas";
const PANEL_W: f32 = 900.0;
const PANEL_H: f32 = 600.0;
/// 视图初始 pan = (40,40)、缩放 1；适配后单卡居中，卡片左上角落在 (170,80)。
const CARD_LEFT: f32 = 100.0;
const CARD_TOP: f32 = 100.0;
const CARD_W: f32 = 360.0;
const CARD_H: f32 = 240.0;
/// 卡头字号固定为屏幕像素（不随缩放变化）。
const CARD_TITLE_SIZE: f32 = 12.0;
/// 标题实际可用宽度 = 卡宽 - 左右内边距与图标位（见画布世界层排版）。
const TITLE_LIMIT: f32 = CARD_W - 36.0;
/// 屏幕宽不超过该值时卡片退化为「卡头 + 居中图标」占位形态。
const COMPACT_CARD_PX: f32 = 120.0;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = atelyx_core::scratch::TempDir::new("canvas-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
    runtime.scan();

    let vault = atelyx_core::scratch::TempDir::new("canvas-vault");
    std::fs::write(vault.join("demo.atlx"), seed_canvas()).expect("写初始画布");

    let state = Arc::new(HostState::default());
    serve(runtime.clone(), state.clone(), vault.to_path_buf());

    if !matches!(runtime.list().into_iter().find(|p| p.id == CANVAS_ID).map(|p| p.status), Some(PluginStatus::Running)) {
        enable(&runtime, CANVAS_ID).await;
    }
    wait_running(&runtime, CANVAS_ID).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    resize(&runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    runtime.notify_surface_attached(CANVAS_ID, SURFACE, SurfaceKind::Panel);
    for _ in 0..20 {
        resize(&runtime);
        runtime.on_open_files_changed();
        tokio::time::sleep(Duration::from_millis(50)).await;
        if runtime
            .surface_frame(CANVAS_ID, SURFACE)
            .is_some_and(|frame| frame.size.is_some_and(|size| (size.w - PANEL_W).abs() < 0.5))
        {
            break;
        }
    }

    // 未打开画布：插件必须提交空态帧，而不是让宿主留着上一张帧
    let empty = wait_frame(&runtime, |frame| has_text(frame, "打开画布"))
        .await
        .expect("未打开画布时应提交空态帧");
    assert_empty_state_centered(&empty, "打开画布", "在文件面板或搜索面板单击一个 .atlx 画布开始编辑。");
    println!("空态：{} 条指令", empty.commands.len());

    // 宿主打开画布后才建会话
    *state.open_files.lock().unwrap() = vec!["demo.atlx".into()];
    runtime.on_open_files_changed();
    // 新会话从默认视口起算：再报一次尺寸，卡片浮层与命中才按面尺寸铺开
    resize(&runtime);

    // 首帧：网格底 + 卡片标题画进主帧（浮层随视口铺开后才有命中区）
    let frame = wait_frame(&runtime, |frame| has_text_prefix(frame, "现有卡") && !frame.hits.is_empty())
        .await
        .unwrap_or_else(|| panic!("载入后应出画布帧；当前无帧"));
    assert!(frame.hits.iter().any(|hit| !hit.id.is_empty()), "应有可命中区域");
    // 长标题按卡宽省略：世界层指令没有逐项裁剪，越界会盖住控件或溢出卡框
    let title = title_text(&frame).expect("应画出卡片标题");
    assert!(title.ends_with('…'), "长标题应省略：{title}");
    assert!(
        text_width(&title, CARD_TITLE_SIZE) <= TITLE_LIMIT + 0.5,
        "标题不应越出卡框：{} > {TITLE_LIMIT}",
        text_width(&title, CARD_TITLE_SIZE)
    );
    assert_eq!(
        title_size(&frame),
        Some(CARD_TITLE_SIZE),
        "卡头字号固定为屏幕像素"
    );
    println!("载入：{} 条指令 / {} 个命中区", frame.commands.len(), frame.hits.len());

    // 右键空白 → 宿主弹菜单 → 选「新建文本卡」：画布内容变脏
    right_click_at(&runtime, 700.0, 200.0);
    wait_menu(&state).await;
    runtime.notify_menu_closed(CANVAS_ID, Some("canvas-menu-new-text".to_string()));
    wait_written(&state, 1).await;
    let two_cards: Value = serde_json::from_str(&written_at(&state, 0)).expect("写盘内容是画布 JSON");
    assert_eq!(two_cards["nodes"].as_array().map(Vec::len), Some(2), "新建卡后应有 2 张卡");
    let dirty = wait_header(&state, |params| {
        params.header.as_ref().is_some_and(|header| header.dirty)
    })
    .await;
    assert!(dirty.is_some(), "新建卡后应声明为脏头部");

    // 拖动第一张卡：从卡头按到右下移 100×60
    let start = (320.0, 210.0);
    let end = (start.0 + 100.0, start.1 + 60.0);
    pointer(&runtime, FrameInput::PointerDown { x: start.0, y: start.1, button: MouseButton::Left, hit: None });
    pointer(&runtime, FrameInput::PointerMove { x: end.0, y: end.1, hit: None });
    pointer(&runtime, FrameInput::PointerUp { x: end.0, y: end.1, button: MouseButton::Left, hit: None });
    wait_written(&state, 2).await;
    let moved: Value = serde_json::from_str(&written_at(&state, 1)).expect("写盘内容是画布 JSON");
    let x = moved["nodes"][0]["x"].as_f64().unwrap_or(0.0);
    assert!(x > CARD_LEFT as f64 + 50.0, "拖动后 x 应移动：{x}");
    assert!(
        wait_header(&state, |params| !params.header.as_ref().is_some_and(|header| header.dirty))
            .await
            .is_some(),
        "落盘完成后应声明为干净头部"
    );
    println!("新建与拖动落盘：通过");

    // 撤销：宿主把画布作用域命令转发为事件，插件回到拖动前的位置并再次落盘
    runtime.notify_plugin_event(
        CANVAS_ID,
        atelyx_core::plugin_runtime::event_name::CANVAS_COMMAND,
        &atelyx_core::plugin_runtime::CanvasCommandParams { command: "undo".to_string() },
    );
    wait_written(&state, 3).await;
    let undone: Value = serde_json::from_str(&written_at(&state, 2)).expect("撤销写盘");
    assert!(
        undone["nodes"][0]["x"].as_f64().unwrap_or(999.0) <= CARD_LEFT as f64 + 0.5,
        "撤销后应回到拖动前的 x"
    );
    println!("撤销：通过");

    // 滚轮方向：协议增量正 = 向下；向上滚（负增量）应放大，字号与缩放无关
    let before_zoom = zoom_percent(&frame).expect("工具条应显示缩放百分比");
    wheel(&runtime, -40.0, false);
    let zoomed = wait_frame(&runtime, |frame| {
        zoom_percent(frame).is_some_and(|pct| pct > before_zoom)
    })
    .await
    .expect("向上滚应放大");
    assert_eq!(
        title_size(&zoomed),
        Some(CARD_TITLE_SIZE),
        "放大后卡头字号不应变化"
    );
    println!("滚轮方向与字号固定：通过（{before_zoom}% → {}%）", zoom_percent(&zoomed).unwrap_or(0));

    // 改面板尺寸不再重置视口：既有缩放应原样保留
    let kept = zoom_percent(&zoomed).unwrap_or(0);
    runtime.notify_frame_input(
        CANVAS_ID,
        SURFACE,
        &FrameInput::Resize {
            width: 1000.0,
            height: 600.0,
        },
    );
    let resized = wait_frame_at_width(&runtime, 1000.0, |frame| {
        zoom_percent(frame).is_some_and(|pct| pct == kept)
    })
    .await
    .expect("改尺寸后应保持既有缩放");
    assert_eq!(title_size(&resized), Some(CARD_TITLE_SIZE), "改尺寸后字号不变");
    runtime.notify_frame_input(
        CANVAS_ID,
        SURFACE,
        &FrameInput::Resize {
            width: PANEL_W,
            height: PANEL_H,
        },
    );
    let restored = wait_frame(&runtime, |frame| zoom_percent(frame) == Some(kept))
        .await
        .expect("尺寸还原后缩放仍应保留");
    println!("改尺寸保持视口：通过（{kept}%）");

    // Ctrl+滚轮：正增量（向下）应把画面向上推（pan.1 减小，卡头屏幕上移）
    let title_y = text_y(&restored).expect("应有卡头标题");
    wheel(&runtime, 40.0, true);
    let panned = wait_frame(&runtime, |frame| {
        text_y(frame).is_some_and(|y| y < title_y - 20.0)
    })
    .await
    .expect("Ctrl+滚轮向下应向上平移画面");
    assert_eq!(zoom_percent(&panned), Some(kept), "Ctrl+滚轮不应改变缩放");
    println!("滚轮平移方向：通过");

    // 阈值 120px：卡宽落在 96–120px 区间时就该占位（旧 96px 阈值下这一档仍会画标题）
    wheel(&runtime, 520.0, false);
    let near = wait_frame(&runtime, |frame| {
        let width = CARD_W * zoom_percent(frame).unwrap_or(0) as f32 / 100.0;
        (96.0..=COMPACT_CARD_PX).contains(&width)
            && title_size(frame).is_none()
            && has_icon_path(frame)
    })
    .await
    .expect("卡宽进入 96–120px 区间时应切成占位形态");
    println!("占位阈值 {COMPACT_CARD_PX}px：通过（{}%）", zoom_percent(&near).unwrap_or(0));

    // 低缩放占位：屏幕宽不超过阈值只画卡头与居中图标，正文与标题不再参与排版
    wheel(&runtime, 1000.0, false);
    let compact = wait_frame(&runtime, |frame| {
        let pct = zoom_percent(frame).unwrap_or(0);
        pct <= 15
            && title_size(frame).is_none()
            && card_rect(frame, CARD_W * pct as f32 / 100.0, CARD_H * pct as f32 / 100.0)
            && has_icon_path(frame)
    })
    .await
    .expect("缩小到下限后应切成占位形态");
    assert!(
        CARD_W * zoom_percent(&compact).unwrap_or(0) as f32 / 100.0 <= COMPACT_CARD_PX,
        "占位形态只在卡宽 ≤ {COMPACT_CARD_PX}px 时出现"
    );
    println!("低缩放占位：通过（{}%）", zoom_percent(&compact).unwrap_or(0));

    disable(&runtime, CANVAS_ID).await;
    let status = runtime
        .list()
        .into_iter()
        .find(|plugin| plugin.id == CANVAS_ID)
        .map(|plugin| plugin.status);
    assert!(matches!(status, Some(PluginStatus::Stopped) | None), "停用后应退出运行态");
    println!("停用：通过");
    runtime.shutdown();
}

fn written_at(state: &HostState, index: usize) -> String {
    state.written.lock().unwrap()[index].clone()
}

fn seed_canvas() -> String {
    // 标题刻意超过卡宽：覆盖世界层单行省略（世界层指令没有逐项裁剪）
    let title = "现有卡 · 一个用于验证标题在卡头内省略而不是溢出卡框的很长很长的标题名称";
    json!({
        "schema": "atelyx-canvas/v1",
        "id": "cv-smoke",
        "title": "demo",
        "nodes": [{
            "id": "n1",
            "type": "text",
            "x": CARD_LEFT,
            "y": CARD_TOP,
            "width": CARD_W,
            "height": CARD_H,
            "data": { "title": title, "bodyMd": "正文" }
        }],
        "edges": [],
        "createdAt": 0,
        "updatedAt": 0
    })
    .to_string()
}

/// 冒烟宿主：记录写盘内容、头部声明与菜单请求，并应答插件的能力调用。
#[derive(Default)]
struct HostState {
    written: Mutex<Vec<String>>,
    headers: Mutex<Vec<PanelHeaderParams>>,
    menus: Mutex<usize>,
    unhandled: Mutex<Vec<String>>,
    vault: Mutex<PathBuf>,
    /// 宿主当前打开的文件：先空后给 `demo.atlx`，用来覆盖「未打开画布」空态。
    open_files: Mutex<Vec<String>>,
}

fn serve(runtime: Arc<PluginRuntime>, state: Arc<HostState>, vault: PathBuf) {
    *state.vault.lock().unwrap() = vault;
    tokio::spawn(async move {
        while let Some(request) = runtime.recv_host_request().await {
            match request {
                HostRequest::MeasureBatch { request_id, params, .. } => {
                    let sizes: Vec<MeasureTextResult> =
                        params.items.iter().map(|item| measure(item)).collect();
                    reply(&runtime, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::QueryTheme { request_id, .. } => {
                    reply(&runtime, request_id, serde_json::to_value(theme()).unwrap());
                }
                HostRequest::QueryFonts { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "families": ["Sans Serif"] }));
                }
                HostRequest::VaultReadText { request_id, path, .. } => {
                    let target = state.vault.lock().unwrap().join(path);
                    match std::fs::read_to_string(&target) {
                        Ok(content) => reply(&runtime, request_id, json!({ "content": content })),
                        Err(error) => runtime.resolve_shell_call(request_id, Err(error.to_string())),
                    }
                }
                HostRequest::VaultWriteText { request_id, path, content, .. } => {
                    let target = state.vault.lock().unwrap().join(path);
                    state.written.lock().unwrap().push(content.clone());
                    let result = std::fs::write(&target, content).map_err(|e| e.to_string());
                    match result {
                        Ok(()) => reply(&runtime, request_id, json!({})),
                        Err(error) => runtime.resolve_shell_call(request_id, Err(error)),
                    }
                }
                HostRequest::HistoryRecord { request_id, .. } => {
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::PanelHeader { request_id, params, .. } => {
                    state.headers.lock().unwrap().push(params);
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::EditorOpenFiles { request_id, .. } => {
                    let paths = state.open_files.lock().unwrap().clone();
                    reply(&runtime, request_id, json!({ "paths": paths }));
                }
                HostRequest::AiAgents { request_id, .. } => {
                    reply(
                        &runtime,
                        request_id,
                        json!({ "agents": [{ "id": "chat", "name": "对话", "builtin": true }] }),
                    );
                }
                HostRequest::AiModels { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "providers": [] }));
                }
                HostRequest::VaultListDirs { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "dirs": ["."] }));
                }
                HostRequest::VaultFiles { request_id, .. } => {
                    reply(
                        &runtime,
                        request_id,
                        serde_json::to_value(FilesResult {
                            files: vec![VaultFileEntry {
                                path: "demo.atlx".to_string(),
                                name: "demo.atlx".to_string(),
                                icon: "shapes".to_string(),
                                openable: true,
                            }],
                        })
                        .unwrap(),
                    );
                }
                HostRequest::MenuOpen { request_id, .. } => {
                    *state.menus.lock().unwrap() += 1;
                    // 菜单走界面服务通道：用错回执通道会让 open_menu 永远等不到结果
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::SpaceRoom { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "kind": "local", "connected": false }));
                }
                HostRequest::CollabPeers { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "connected": false }));
                }
                HostRequest::CollabSend { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "queued": false }));
                }
                other => {
                    eprintln!("未处理的宿主请求：{other:?}");
                    state.unhandled.lock().unwrap().push(format!("{other:?}"));
                }
            }
        }
    });
}

fn measure(item: &atelyx_plugin_protocol::method::MeasureItem) -> MeasureTextResult {
    let layout = item.layout.unwrap_or(TextLayout { line_height: None, wrap: None, shaping: None });
    let factor = layout.line_height.unwrap_or(1.2);
    let line_height = item.size * factor;
    let wrap_w = item.max_width.unwrap_or(f32::MAX);
    let mut segments = Vec::new();
    for segment in item.text.split('\n') {
        let mut current = String::new();
        let mut width = 0.0f32;
        for ch in segment.chars() {
            let advance = item.size * if ch.is_ascii() { 0.55 } else { 1.0 };
            if !current.is_empty() && width + advance > wrap_w {
                segments.push(atelyx_plugin_protocol::method::MeasureSegment {
                    text: std::mem::take(&mut current),
                    width,
                });
                width = 0.0;
            }
            current.push(ch);
            width += advance;
        }
        segments.push(atelyx_plugin_protocol::method::MeasureSegment { text: current, width });
    }
    let width = segments.iter().map(|seg| seg.width).fold(0.0, f32::max);
    let lines = segments.len().max(1) as u32;
    MeasureTextResult { width, height: line_height * lines as f32, lines, segments: Some(segments) }
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

fn resize(runtime: &PluginRuntime) {
    runtime.notify_frame_input(CANVAS_ID, SURFACE, &FrameInput::Resize { width: PANEL_W, height: PANEL_H });
}

fn pointer(runtime: &PluginRuntime, input: FrameInput) {
    runtime.notify_frame_input(CANVAS_ID, SURFACE, &input);
}

fn right_click_at(runtime: &PluginRuntime, x: f32, y: f32) {
    for input in [
        FrameInput::PointerDown { x, y, button: MouseButton::Right, hit: None },
        FrameInput::PointerUp { x, y, button: MouseButton::Right, hit: None },
    ] {
        runtime.notify_frame_input(CANVAS_ID, SURFACE, &input);
    }
}

fn reply(runtime: &PluginRuntime, request_id: u64, result: Value) {
    runtime.resolve_shell_call(request_id, Ok(result));
}

fn has_text(frame: &SurfaceFrame, wanted: &str) -> bool {
    frame
        .commands
        .iter()
        .any(|command| matches!(command, DrawCommand::Text { content, .. } if content == wanted))
}

fn has_text_prefix(frame: &SurfaceFrame, prefix: &str) -> bool {
    frame.commands.iter().any(
        |command| matches!(command, DrawCommand::Text { content, .. } if content.starts_with(prefix))
    )
}

/// 卡头标题（世界层，内容是省略后的文本）。
fn title_command(frame: &SurfaceFrame) -> Option<(&str, f32, f32)> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { content, x: _, y, size, .. } if content.starts_with("现有卡") => {
            Some((content.as_str(), *size, *y))
        }
        _ => None,
    })
}

fn title_text(frame: &SurfaceFrame) -> Option<String> {
    title_command(frame).map(|(content, _, _)| content.to_string())
}

fn title_size(frame: &SurfaceFrame) -> Option<f32> {
    title_command(frame).map(|(_, size, _)| size)
}

fn text_y(frame: &SurfaceFrame) -> Option<f32> {
    title_command(frame).map(|(_, _, y)| y)
}

/// 工具条上的缩放百分比。
fn zoom_percent(frame: &SurfaceFrame) -> Option<i32> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { content, .. } if content.ends_with('%') => {
            content.trim_end_matches('%').parse().ok()
        }
        _ => None,
    })
}

/// 卡片主体矩形（宽高按当前缩放后的尺寸）。
fn card_rect(frame: &SurfaceFrame, w: f32, h: f32) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command, DrawCommand::Rect { w: rw, h: rh, .. }
            if (rw - w).abs() < 1.0 && (rh - h).abs() < 1.0)
    })
}

fn has_icon_path(frame: &SurfaceFrame) -> bool {
    frame
        .commands
        .iter()
        .any(|command| matches!(command, DrawCommand::Path { .. }))
}

/// 与宿主量算同一口径的宽度估算（ASCII 0.55 字宽，其余 1 字宽）。
fn text_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|ch| size * if ch.is_ascii() { 0.55 } else { 1.0 })
        .sum()
}

fn wheel(runtime: &PluginRuntime, delta_y: f32, control: bool) {
    let mut modifiers = Modifiers::default();
    modifiers.control = control;
    // 空白世界区（避开卡片正文浮层与小地图）：浮层上的滚轮归控件层滚动
    runtime.notify_frame_input(
        CANVAS_ID,
        SURFACE,
        &FrameInput::Wheel {
            x: 600.0,
            y: 500.0,
            delta_x: 0.0,
            delta_y,
            modifiers,
            hit: None,
        },
    );
}

/// 空态：标题 / 说明按面宽居中，内容块（64 图标 + 两侧 16 间距 + 两行文字）垂直中心落在面中心。
fn assert_empty_state_centered(frame: &SurfaceFrame, title: &str, desc: &str) {
    let size = frame.size.expect("整面帧必须带尺寸");
    let text = |wanted: &str| {
        frame.commands.iter().find_map(|command| match command {
            DrawCommand::Text { content, x, y, size, max_width, align, .. } if content == wanted => {
                Some((*x, *y, *size, *max_width, *align))
            }
            _ => None,
        }).unwrap_or_else(|| panic!("空态缺少文字：{wanted}"))
    };
    let title = text(title);
    let desc = text(desc);
    for (label, (x, _, _, max_width, align)) in [("标题", title), ("说明", desc)] {
        assert_eq!(max_width, Some(size.w), "空态{label}应按面宽对齐");
        assert_eq!(align, Some(TextAlign::Center), "空态{label}应居中");
        assert!(x.abs() < 0.5, "空态{label}对齐盒的左缘应在面左缘");
    }
    let block = 64.0 + 16.0 + title.2 * 1.2 + 16.0 + desc.2 * 1.2;
    let top = (size.h - block) / 2.0;
    assert!((title.1 - (top + 64.0 + 16.0)).abs() < 0.5, "空态标题应垂直居中");
    let desc_y = top + 64.0 + 16.0 + title.2 * 1.2 + 16.0;
    assert!((desc.1 - desc_y).abs() < 0.5, "空态说明应垂直居中");
    assert!(((top + desc_y + desc.2 * 1.2) / 2.0 - size.h / 2.0).abs() < 0.5, "空态内容块应垂直居中");
}

async fn wait_menu(state: &HostState) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while *state.menus.lock().unwrap() == 0 {
        assert!(Instant::now() < deadline, "右键空白应请求宿主弹菜单");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_written(state: &HostState, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.written.lock().unwrap().len() < count {
        assert!(
            Instant::now() < deadline,
            "宿主未收到写盘回执（累计 {}）；未接线的宿主请求：{:?}",
            state.written.lock().unwrap().len(),
            state.unhandled.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_header(
    state: &HostState,
    accept: impl Fn(&PanelHeaderParams) -> bool,
) -> Option<PanelHeaderParams> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(params) = state.headers.lock().unwrap().iter().rev().find(|params| accept(params)) {
            return Some(params.clone());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_frame(
    runtime: &PluginRuntime,
    accept: impl Fn(&SurfaceFrame) -> bool,
) -> Option<SurfaceFrame> {
    wait_frame_at_width(runtime, PANEL_W, accept).await
}

async fn wait_frame_at_width(
    runtime: &PluginRuntime,
    width: f32,
    accept: impl Fn(&SurfaceFrame) -> bool,
) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(frame) = runtime.surface_frame(CANVAS_ID, SURFACE) {
            let sized = frame.size.is_some_and(|size| (size.w - width).abs() < 0.5);
            if sized && accept(&frame) {
                return Some((*frame).clone());
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

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

async fn wait_running(runtime: &PluginRuntime, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = runtime.list().into_iter().find(|plugin| plugin.id == id).map(|p| p.status);
        if matches!(status, Some(PluginStatus::Running)) {
            return;
        }
        assert!(Instant::now() < deadline, "{id} 未进入运行态：{status:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
