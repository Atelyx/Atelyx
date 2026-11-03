//! 表格插件无界面冒烟：经真实 `PluginRuntime` 加载、挂面、尺寸下发、取帧，覆盖首次载入、
//! 单元格点选与编辑提交、防抖落盘、新行添加、撤销、面板头部脏标记声明与停用收尾。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, MouseButton, PanelHeaderParams, PluginRuntime,
    PluginStatus, SurfaceKind,
};
use atelyx_plugin_protocol::frame::{PopupLayer, SurfaceFrame, SurfacePopup};
use atelyx_plugin_protocol::method::{
    FontSizes, Radii, ThemeColors, ThemeResult,
};
use atelyx_plugin_protocol::paint::{DrawCommand, TextLayout};

const TABLES_ID: &str = "atelyx.tables";
const SURFACE: &str = "table";
/// 冒烟下发的面尺寸（与 SDK 首帧占位尺寸不同，才能证明按尺寸重排生效）。
const PANEL_W: f32 = 900.0;
const PANEL_H: f32 = 600.0;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = atelyx_core::scratch::TempDir::new("tables-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
    runtime.scan();

    // 临时仓库：一张两行一列的表格
    let vault = atelyx_core::scratch::TempDir::new("tables-vault");
    let table_path = vault.join("demo.atb");
    std::fs::write(&table_path, seed_table()).expect("写初始表格");

    let state = Arc::new(HostState::default());
    serve(runtime.clone(), state.clone(), vault.to_path_buf());

    enable(&runtime, TABLES_ID).await;
    wait_running(&runtime, TABLES_ID).await;
    // 等 on_ready 里的贡献注册与事件订阅落地，否则接下来的广播收不到
    tokio::time::sleep(Duration::from_millis(300)).await;
    runtime.notify_surface_attached(TABLES_ID, SURFACE, SurfaceKind::TableView);
    // 反复补发尺寸与打开文件事件：插件订阅 / 面挂载可能晚于第一笔
    for _ in 0..20 {
        resize(&runtime, TABLES_ID, SURFACE);
        runtime.on_open_files_changed();
        tokio::time::sleep(Duration::from_millis(50)).await;
        if runtime
            .surface_frame(TABLES_ID, SURFACE)
            .is_some_and(|frame| frame.size.is_some_and(|size| size.w > 1.0))
        {
            break;
        }
    }

    // 首帧：表头 + 两行正文都画出来（文字量算走宿主的逐行行段）
    let frame = wait_frame(&runtime, |frame| has_text(frame, "名称") && has_text(frame, "甲"))
        .await
        .unwrap_or_else(|| {
            if let Some(frame) = runtime.surface_frame(TABLES_ID, SURFACE) {
                let texts: Vec<String> = frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text { content, .. } => Some(content.clone()),
                        _ => None,
                    })
                    .collect();
                panic!("载入后应出网格帧；当前帧文字：{texts:?}，尺寸：{:?}", frame.size);
            }
            panic!("载入后应出网格帧；还没有任何帧");
        });
    assert!(has_text(&frame, "乙"), "第二行正文应出现");
    assert!(frame.hits.iter().any(|hit| hit.id.starts_with("h:")), "表头应有光标区");
    println!("载入：{} 条指令 / {} 个命中区", frame.commands.len(), frame.hits.len());

    // 点第一格 → 再点一次已在选中的格弹格式气泡（窗口级浮层，气泡帧里有 B / I / U / S 字样）
    let cell = cell_point(0, 0);
    click_at(&runtime, cell.0, cell.1);
    // 隔开双击判定窗口：第二下必须是独立单击，否则会走「双击进编辑」
    tokio::time::sleep(Duration::from_millis(650)).await;
    click_at(&runtime, cell.0, cell.1);
    let bubble = wait_frame(&runtime, |frame| {
        frame.popup.as_ref().is_some_and(|popup| {
            popup.layer == PopupLayer::Window
                && popup_has_text(popup, "B")
                && popup_has_text(popup, "S")
        })
    })
    .await;
    let bubble = bubble.expect("重按已选中格应在窗口级浮层弹格式气泡");
    assert!(!has_text(&bubble, "S"), "格式气泡不应再画进主帧命令");

    // 双击进编辑 → 输入替换文本 → 回车提交（先全选再替换：Ctrl+A 后打字）
    double_click_at(&runtime, cell.0, cell.1);
    let _ = wait_frame(&runtime, |_| true).await;
    send_key(&runtime, "a", true);
    type_text(&runtime, "丙");
    send_key(&runtime, "Enter", false);
    let edited = wait_frame(&runtime, |frame| has_text(frame, "丙")).await;
    assert!(edited.is_some(), "提交后网格应显示新值");

    // 防抖落盘：等宿主收到写盘，内容里带新值
    wait_written(&state, 1).await;
    let written = state.written.lock().unwrap().last().cloned().expect("应有写盘内容");
    assert!(written.contains("丙"), "落盘内容应包含新值：{written}");
    println!("编辑与落盘：通过");

    // 头部声明：编辑期间置脏（在途草稿也计入），落盘完成后回落干净
    let dirty_header = wait_header(&state, |params| {
        params
            .header
            .as_ref()
            .is_some_and(|header| header.dirty && header.status.is_some())
    })
    .await;
    assert!(dirty_header.is_some(), "编辑后应声明为脏头部");
    let header = wait_header(&state, |params| {
        params
            .header
            .as_ref()
            .is_some_and(|header| !header.dirty && header.status.is_none())
    })
    .await;
    assert!(header.is_some(), "落盘完成后应声明为干净头部");

    // 新行条：帧底部的「新行」文字位置点一下，行数 +1（头部统计随声明更新）
    let (x, y) = new_row_point();
    click_at(&runtime, x, y);
    let grown = wait_header(&state, |params| {
        params.header.as_ref().is_some_and(|header| {
            header
                .texts
                .last()
                .is_some_and(|text| text.starts_with("3 行"))
        })
    })
    .await;
    assert!(grown.is_some(), "新增一行后头部统计应为 3 行");
    wait_written(&state, 2).await;
    println!("新行与统计：通过");

    // 撤销：Ctrl+Z 回到新增前的行数（头部统计回落 2 行）
    send_key(&runtime, "z", true);
    let undone = wait_header(&state, |params| {
        params.header.as_ref().is_some_and(|header| {
            header
                .texts
                .last()
                .is_some_and(|text| text.starts_with("2 行"))
        })
    })
    .await;
    assert!(undone.is_some(), "撤销后行数应回到 2 行");
    wait_written(&state, 3).await;
    println!("撤销：通过");

    // 停用：插件退出运行态
    disable(&runtime, TABLES_ID).await;
    let status = runtime
        .list()
        .into_iter()
        .find(|plugin| plugin.id == TABLES_ID)
        .map(|plugin| plugin.status);
    assert!(matches!(status, Some(PluginStatus::Stopped) | None), "停用后应退出运行态");
    println!("停用：通过");
    runtime.shutdown();
}

/// 初始表格内容（schema / 字段 / 两行）。
fn seed_table() -> String {
    serde_json::json!({
        "schema": "atelyx-table/v1",
        "id": "tb-smoke",
        "title": "demo",
        "fields": [{ "id": "f1", "name": "名称", "type": "text" }],
        "rows": [
            { "id": "r1", "values": { "f1": "甲" } },
            { "id": "r2", "values": { "f1": "乙" } }
        ],
        "createdAt": 0,
        "updatedAt": 0
    })
    .to_string()
}

/// 冒烟宿主：记录写盘内容与头部声明，并应答插件的能力调用。
#[derive(Default)]
struct HostState {
    written: Mutex<Vec<String>>,
    headers: Mutex<Vec<PanelHeaderParams>>,
    /// 冒烟宿主未接线的请求（等待失败时列出，便于定位缺口）。
    unhandled: Mutex<Vec<String>>,
    vault: Mutex<PathBuf>,
}

fn serve(runtime: Arc<PluginRuntime>, state: Arc<HostState>, vault: PathBuf) {
    *state.vault.lock().unwrap() = vault;
    tokio::spawn(async move {
        while let Some(request) = runtime.recv_host_request().await {
            match request {
                HostRequest::MeasureBatch { request_id, params, .. } => {
                    let sizes: Vec<MeasureTextResult> =
                        params.items.iter().map(|item| measure(item)).collect();
                    reply(&runtime, request_id, serde_json::json!({ "sizes": sizes }));
                }
                HostRequest::QueryTheme { request_id, .. } => {
                    reply(&runtime, request_id, serde_json::to_value(theme()).unwrap());
                }
                HostRequest::QueryFonts { request_id, .. } => {
                    reply(
                        &runtime,
                        request_id,
                        json!({ "families": ["Sans Serif", "Sarasa Mono SC"] }),
                    );
                }
                HostRequest::VaultReadText { request_id, path, .. } => {
                    let target = state.vault.lock().unwrap().join(path);
                    match std::fs::read_to_string(&target) {
                        Ok(content) => reply(&runtime, request_id, json!({ "content": content })),
                        Err(error) => {
                            runtime.resolve_shell_call(request_id, Err(error.to_string()))
                        }
                    }
                }
                HostRequest::VaultWriteText { request_id, path, content, .. } => {
                    let target = state.vault.lock().unwrap().join(path);
                    state.written.lock().unwrap().push(content.clone());
                    let result = std::fs::write(&target, content).map_err(|error| error.to_string());
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
                HostRequest::SpaceRoom { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "kind": "local", "connected": false }));
                }
                HostRequest::EditorOpenFiles { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "paths": ["demo.atb"] }));
                }
                HostRequest::MenuOpen { request_id, .. } => {
                    reply(&runtime, request_id, json!({}));
                }
                HostRequest::CollabSend { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "queued": false }));
                }
                other => {
                    // 未接线时记下来：等待超时会把缺少的请求一并点名，不误报成时序问题
                    eprintln!("未处理的宿主请求：{other:?}");
                    state.unhandled.lock().unwrap().push(format!("{other:?}"));
                }
            }
        }
    });
}

/// 量算应答：按估算字宽给宽高，并按硬换行切出行段（插件绘制依赖行段）。
fn measure(item: &atelyx_plugin_protocol::method::MeasureItem) -> MeasureTextResult {
    let layout = item.layout.unwrap_or(TextLayout {
        line_height: None,
        wrap: None,
        shaping: None,
    });
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
    MeasureTextResult {
        width,
        height: line_height * lines as f32,
        lines,
        segments: Some(segments),
    }
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

fn resize(runtime: &PluginRuntime, plugin: &str, surface: &str) {
    runtime.notify_frame_input(plugin, surface, &FrameInput::Resize { width: PANEL_W, height: PANEL_H });
}

fn click_at(runtime: &PluginRuntime, x: f32, y: f32) {
    for input in [
        FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: None },
        FrameInput::PointerUp { x, y, button: MouseButton::Left, hit: None },
    ] {
        runtime.notify_frame_input(TABLES_ID, SURFACE, &input);
    }
}

fn double_click_at(runtime: &PluginRuntime, x: f32, y: f32) {
    runtime.notify_frame_input(
        TABLES_ID,
        SURFACE,
        &FrameInput::DoubleClick { x, y, button: MouseButton::Left, hit: None },
    );
    runtime.notify_frame_input(
        TABLES_ID,
        SURFACE,
        &FrameInput::PointerUp { x, y, button: MouseButton::Left, hit: None },
    );
}

fn type_text(runtime: &PluginRuntime, text: &str) {
    runtime.notify_frame_input(
        TABLES_ID,
        SURFACE,
        &FrameInput::Text { text: text.into(), composing: false, caret: None, candidates: None },
    );
}

fn send_key(runtime: &PluginRuntime, named: &str, control: bool) {
    use atelyx_plugin_protocol::frame::{Key, KeyAction, Modifiers, NamedKey};
    let key = match named {
        "Enter" => Key::Named { name: NamedKey::Enter },
        other => Key::Character { text: other.to_string() },
    };
    runtime.notify_frame_input(
        TABLES_ID,
        SURFACE,
        &FrameInput::Key {
            key,
            action: KeyAction::Press,
            modifiers: Modifiers { shift: false, control, alt: false, meta: false },
            text: None,
        },
    );
}

/// 单元格中心：表头 32 + 行号列 40，列宽按字段名自适应（「名称」约 120px），行高 32。
fn cell_point(row: usize, field: usize) -> (f32, f32) {
    (40.0 + 60.0 + field as f32 * 120.0, 32.0 + 16.0 + row as f32 * 32.0)
}

/// 新行条中心：两行内容之后（表头 32 + 两行 64 + 条高一半）。
fn new_row_point() -> (f32, f32) {
    // 新行条在内容末尾（两行之后），横坐标落在内容区里（右侧是加列占位）
    (100.0, 32.0 + 2.0 * 32.0 + 16.0)
}

fn reply(runtime: &PluginRuntime, request_id: u64, result: Value) {
    runtime.resolve_shell_call(request_id, Ok(result));
}

fn has_text(frame: &SurfaceFrame, wanted: &str) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command, DrawCommand::Text { content, .. } if content == wanted)
    })
}

fn popup_has_text(popup: &SurfacePopup, wanted: &str) -> bool {
    popup.commands.iter().any(|command| {
        matches!(command, DrawCommand::Text { content, .. } if content == wanted)
    })
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
        if Instant::now() >= deadline {
            panic!("{id} 未进入运行态：{status:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_frame(
    runtime: &PluginRuntime,
    accept: impl Fn(&SurfaceFrame) -> bool,
) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(frame) = runtime.surface_frame(TABLES_ID, SURFACE) {
            let sized = frame
                .size
                .is_some_and(|size| (size.w - PANEL_W).abs() < 0.5 && (size.h - PANEL_H).abs() < 0.5);
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
