//! 搜索插件无界面冒烟：经真实 `PluginRuntime` 加载、挂面、尺寸下发、取帧，覆盖输入过滤、
//! 清空回焦、行点击回发宿主、以及清单变化事件驱动的重取。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FilesResult, FrameInput, HostRequest, MeasureTextResult, MouseButton, PluginRuntime,
    PluginStatus, SurfaceKind, VaultFileEntry,
};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};
use atelyx_plugin_protocol::paint::DrawCommand;

const SEARCH_ID: &str = "atelyx.search";
const SEARCH_SURFACE: &str = "search";
/// 冒烟下发的面板尺寸（与 SDK 首帧占位尺寸不同，才能证明按尺寸重排生效）。
const PANEL_W: f32 = 900.0;
const PANEL_H: f32 = 600.0;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    assert_eq!(
        format!("plugin:{SEARCH_ID}:{SEARCH_SURFACE}"),
        atelyx_core::layout::SEARCH_VIEW
    );
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    // 在临时副本上跑：停用与卸载会真删插件目录
    let root = std::env::temp_dir().join(format!("atelyx-search-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.clone(), "0.6.5");
    runtime.scan();

    let state = HostState::new();
    state.fail_once.store(true, Ordering::SeqCst);
    state.files.lock().unwrap().extend([
        file("notes/方案.md", "sticky-note", true),
        file("plan.atb", "table", true),
        file("media/plan.png", "image", false),
    ]);
    serve(runtime.clone(), state.clone());

    enable(&runtime, SEARCH_ID).await;
    wait_running(&runtime, SEARCH_ID).await;
    runtime.notify_surface_attached(SEARCH_ID, SEARCH_SURFACE, SurfaceKind::Panel);
    resize(&runtime, SEARCH_ID, SEARCH_SURFACE);

    // 首份清单失败：给可读原因与重试，而不是误报「还没有可搜索的文件」
    let failed = wait_frame(&runtime, |frame| has_text(frame, "搜索清单读取失败"))
        .await
        .expect("首份清单失败应出失败态");
    assert!(failed.hits.iter().any(|hit| hit.id == "search-retry"));
    click_hit(&runtime, &failed, "search-retry");
    let frame = wait_frame(&runtime, |frame| has_text(frame, "共 3 个文件"))
        .await
        .expect("重试后应出全量清单帧");
    assert!(frame.hits.iter().any(|hit| hit.id == "search-input"));
    assert!(frame.hits.iter().any(|hit| hit.id.starts_with("open:")));
    println!("挂面：{} 条指令 / {} 个命中区", frame.commands.len(), frame.hits.len());

    // 输入过滤：点输入框取焦 → 提交查询词 → 计数行与行集都按命中重排
    click_hit(&runtime, &frame, "search-input");
    type_text(&runtime, "plan");
    let filtered = wait_frame(&runtime, |frame| has_text(frame, "2 个结果"))
        .await
        .expect("输入后应按命中出帧");
    assert!(has_text(&filtered, "plan.atb"), "命中的文件名应出现");
    assert!(has_text(&filtered, "plan.png"), "路径命中的附件也应出现");
    assert!(!has_text(&filtered, "方案.md"), "未命中的行不应出现");
    println!("输入过滤：通过");

    // 清空钮：清空查询词并回焦输入框（回焦后再次提交的文本接在末尾）
    click_hit(&runtime, &filtered, "search-clear");
    let cleared = wait_frame(&runtime, |frame| has_text(frame, "共 3 个文件"))
        .await
        .expect("清空后应回到全量清单");
    assert!(has_text(&cleared, "方案.md"), "清空后全量清单应恢复");
    type_text(&runtime, "-x");
    let refocused = wait_frame(&runtime, |frame| has_text(frame, "-x"))
        .await
        .expect("清空后应仍保留输入焦点");
    assert!(has_text(&refocused, "无匹配文件"));
    click_hit(&runtime, &refocused, "search-clear");
    let cleared = wait_frame(&runtime, |frame| has_text(frame, "共 3 个文件"))
        .await
        .expect("二次清空后应回到全量清单");
    println!("清空回焦：通过");

    // 行点击 → 宿主按扩展名打开该文件（清单树序的第 0 行）
    let row = open_hit(&cleared, 0);
    click_hit(&runtime, &cleared, &row);
    wait_opens(&state, 1).await;
    let opens = state.opens.lock().unwrap().clone();
    assert_eq!(opens, vec![("notes/方案.md".to_string(), true)]);
    println!("行点击：打开文件请求已回发");

    // 宿主清单变化 → 广播 → 插件重取并重出帧
    state.files.lock().unwrap().truncate(1);
    runtime.on_files_changed();
    let refreshed = wait_frame(&runtime, |frame| has_text(frame, "共 1 个文件"))
        .await
        .expect("清单变化后应重取");
    assert!(has_text(&refreshed, "方案.md"));
    println!("事件重取：通过");

    runtime.set_enabled(SEARCH_ID, false).expect("停用");
    let status = runtime
        .list()
        .into_iter()
        .find(|plugin| plugin.id == SEARCH_ID)
        .map(|plugin| plugin.status);
    assert_eq!(status, Some(PluginStatus::Stopped), "停用后应回到 Stopped");
    println!("停用：通过");

    runtime.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// 宿主侧回执：主题 / 量算 / 文件清单 / 打开请求记录。
#[derive(Clone)]
struct HostState {
    files: Arc<Mutex<Vec<VaultFileEntry>>>,
    opens: Arc<Mutex<Vec<(String, bool)>>>,
    /// 首次取清单失败一次，用来走「失败态 → 重试」的真实路径。
    fail_once: Arc<AtomicBool>,
}

impl HostState {
    fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(Vec::new())),
            opens: Arc::new(Mutex::new(Vec::new())),
            fail_once: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn file(path: &str, icon: &str, openable: bool) -> VaultFileEntry {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    VaultFileEntry { path: path.into(), name, icon: icon.into(), openable }
}

fn serve(runtime: Arc<PluginRuntime>, state: HostState) {
    tokio::spawn(async move {
        while let Some(request) = runtime.recv_host_request().await {
            match request {
                HostRequest::QueryTheme { request_id, .. } => {
                    reply(&runtime, request_id, serde_json::to_value(theme()).unwrap());
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
                    reply(&runtime, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::VaultFiles { request_id, .. } => {
                    if state.fail_once.swap(false, Ordering::SeqCst) {
                        runtime.resolve_shell_call(request_id, Err("冒烟：首次取清单失败".into()));
                        continue;
                    }
                    let files = state.files.lock().unwrap().clone();
                    reply(
                        &runtime,
                        request_id,
                        serde_json::to_value(FilesResult { files }).unwrap(),
                    );
                }
                HostRequest::EditorOpenFile { request_id, path, reveal, .. } => {
                    state.opens.lock().unwrap().push((path, reveal));
                    reply(&runtime, request_id, json!({}));
                }
                other => eprintln!("未处理的宿主请求：{other:?}"),
            }
        }
    });
}

async fn enable(runtime: &Arc<PluginRuntime>, id: &str) {
    let runtime = runtime.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || runtime.set_enabled(&id, true))
        .await
        .expect("启用任务不应 panic")
        .expect("启用预置插件");
}

/// 下发面板尺寸（宿主第一次绘制前就会发这一笔；插件必须据此重出帧）。
fn resize(runtime: &PluginRuntime, plugin: &str, surface: &str) {
    runtime.notify_frame_input(
        plugin,
        surface,
        &FrameInput::Resize { width: PANEL_W, height: PANEL_H },
    );
}

/// 按下 + 抬起一条命中区（宿主判定点击的同一路径）。
fn click_hit(runtime: &PluginRuntime, frame: &SurfaceFrame, hit_id: &str) {
    let hit = frame.hits.iter().find(|hit| hit.id == hit_id).expect("命中区应存在");
    let (x, y) = (hit.x + hit.w / 2.0, hit.y + hit.h / 2.0);
    for input in [
        FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: Some(hit_id.to_string()) },
        FrameInput::PointerUp { x, y, button: MouseButton::Left, hit: Some(hit_id.to_string()) },
    ] {
        runtime.notify_frame_input(SEARCH_ID, SEARCH_SURFACE, &input);
    }
}

/// 向已聚焦的输入框提交一段文本（输入法提交路径）。
fn type_text(runtime: &PluginRuntime, text: &str) {
    runtime.notify_frame_input(
        SEARCH_ID,
        SEARCH_SURFACE,
        &FrameInput::Text { text: text.into(), composing: false, caret: None, candidates: None },
    );
}

/// 取某一行卡片的命中区 id（`open:<世代>:<行号>`）。
fn open_hit(frame: &SurfaceFrame, index: usize) -> String {
    frame
        .hits
        .iter()
        .find(|hit| hit.id.ends_with(&format!(":{index}")))
        .expect("应有可点行")
        .id
        .clone()
}

async fn wait_opens(state: &HostState, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.opens.lock().unwrap().len() < count {
        assert!(Instant::now() < deadline, "宿主未收到插件的打开请求");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn reply(runtime: &PluginRuntime, request_id: u64, result: Value) {
    runtime.resolve_shell_call(request_id, Ok(result));
}

/// 帧里是否画过这段文字（计数行 / 文件名断言用）。
fn has_text(frame: &SurfaceFrame, wanted: &str) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command, DrawCommand::Text { content, .. } if content == wanted)
    })
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

/// 等一张按面板尺寸排版且满足 `accept` 的帧：尺寸不对的帧说明 Resize 没被接住。
async fn wait_frame(
    runtime: &PluginRuntime,
    accept: impl Fn(&SurfaceFrame) -> bool,
) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(frame) = runtime.surface_frame(SEARCH_ID, SEARCH_SURFACE) {
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
