//! 最近打开 / 仓库历史两个预置插件的无界面冒烟：经真实 `PluginRuntime` 加载、挂面、尺寸下发、
//! 收帧、点击回发宿主请求、失败重试与事件驱动重取。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, MouseButton, PluginRuntime, PluginStatus,
    RecentEntry, RecentKind, RecentListResult, SurfaceKind,
};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};

const RECENT_ID: &str = "atelyx.recent";
const RECENT_SURFACE: &str = "recent";
const HISTORY_ID: &str = "atelyx.history";
const HISTORY_SURFACE: &str = "repohistory";
/// 冒烟下发的面板尺寸（与 SDK 首帧占位尺寸不同，才能证明按尺寸重排生效）。
const PANEL_W: f32 = 900.0;
const PANEL_H: f32 = 600.0;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // 布局常量由插件 id + 贡献 id 拼出：这里锁住两侧字符串没有漂移
    assert_eq!(
        format!("plugin:{RECENT_ID}:{RECENT_SURFACE}"),
        atelyx_core::layout::RECENT_VIEW
    );
    assert_eq!(
        format!("plugin:{HISTORY_ID}:{HISTORY_SURFACE}"),
        atelyx_core::layout::REPO_HISTORY_VIEW
    );
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    // 在临时副本上跑：卸载会真删插件目录（也验证 Windows 上动态库能随实例释放）
    let root = std::env::temp_dir().join(format!("atelyx-views-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.clone(), "0.6.5");
    runtime.scan();

    let state = HostState::new();
    state.recent_entries.lock().unwrap().extend([
        recent("notes/方案.md", RecentKind::Note, 1_700_000_100),
        recent("plan.atb", RecentKind::Table, 1_700_000_000),
    ]);
    // 首次聚合失败一次，用来走「失败态 → 重试」的真实路径
    state.history_fail_once.store(true, Ordering::SeqCst);
    serve(runtime.clone(), state.clone());

    for id in [RECENT_ID, HISTORY_ID] {
        enable(&runtime, id).await;
    }
    wait_running(&runtime, RECENT_ID).await;
    wait_running(&runtime, HISTORY_ID).await;

    runtime.notify_surface_attached(RECENT_ID, RECENT_SURFACE, SurfaceKind::Panel);
    runtime.notify_surface_attached(HISTORY_ID, HISTORY_SURFACE, SurfaceKind::Panel);
    for (id, surface) in [(RECENT_ID, RECENT_SURFACE), (HISTORY_ID, HISTORY_SURFACE)] {
        resize(&runtime, id, surface);
    }

    // 按面板尺寸排的帧才算数：插件收到 Resize 后必须重出帧
    let recent_frame = wait_frame(&runtime, RECENT_ID, RECENT_SURFACE, |_| true)
        .await
        .expect("最近打开挂面后应有帧");
    let error_frame = wait_frame(&runtime, HISTORY_ID, HISTORY_SURFACE, |frame| {
        frame.hits.iter().any(|hit| hit.id == "retry")
    })
    .await
    .expect("首次聚合失败应给出重试入口");
    assert!(error_frame.hits.iter().any(|hit| hit.id == "retry"));
    println!(
        "挂面：最近打开 {} 条指令 / 历史失败态 {} 条指令",
        recent_frame.commands.len(),
        error_frame.commands.len()
    );

    // 重试：必须带 refresh（宿主据此丢弃聚合缓存重扫）
    click(&runtime, HISTORY_ID, HISTORY_SURFACE, &error_frame, "retry");
    let history_frame = wait_frame(&runtime, HISTORY_ID, HISTORY_SURFACE, |frame| {
        frame.hits.iter().any(|hit| hit.id.starts_with("open:"))
    })
    .await
    .expect("重试后应拿到分组帧");
    {
        let refreshes = state.history_refreshes.lock().unwrap();
        assert_eq!(refreshes.first().copied(), Some(false), "首载不带 refresh");
        assert_eq!(refreshes.last().copied(), Some(true), "重试带 refresh");
    }
    assert!(state.history_calls.load(Ordering::SeqCst) >= 2);
    assert!(recent_frame.hits.iter().any(|hit| hit.id.starts_with("open:")));
    println!("失败重试：通过");

    // 点击回发：文件按扩展名打开，历史卡片打开该文件的历史弹层（按行号点第二张）
    click(&runtime, RECENT_ID, RECENT_SURFACE, &recent_frame, &open_hit(&recent_frame, 0));
    click(&runtime, HISTORY_ID, HISTORY_SURFACE, &history_frame, &open_hit(&history_frame, 1));
    wait_opens(&state, 2).await;
    let opens = state.opens.lock().unwrap().clone();
    assert!(opens.contains(&("openFile".into(), "notes/方案.md".into())), "{opens:?}");
    assert!(opens.contains(&("historyOpen".into(), "table:plan.atb".into())), "{opens:?}");
    // 列表入口只打开文件：不改布局（不新增 / 不切换视图标签）
    assert_eq!(*state.open_reveals.lock().unwrap(), vec![false]);
    println!("点击：打开文件与历史弹层请求都已回发");

    // 宿主记录变化 → 广播 → 插件重取并重出帧（条数变少）
    state.recent_entries.lock().unwrap().truncate(1);
    runtime.on_recent_changed();
    let frame = wait_frame(&runtime, RECENT_ID, RECENT_SURFACE, |frame| {
        !frame.hits.iter().any(|hit| hit.id.contains(":1"))
    })
    .await
    .expect("最近打开未按事件重取");
    assert!(frame.hits.iter().any(|hit| hit.id.starts_with("open:")));
    println!("事件重取：通过");

    // 历史版本集合变化 → 广播 → 插件重取（宿主已丢弃缓存，不带 refresh）
    let before = state.history_calls.load(Ordering::SeqCst);
    runtime.on_history_changed();
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.history_calls.load(Ordering::SeqCst) <= before {
        assert!(Instant::now() < deadline, "仓库历史未按事件重取");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        state.history_refreshes.lock().unwrap().last().copied(),
        Some(false),
        "事件重取吃宿主已失效的缓存，不带 refresh"
    );
    println!("历史重取：通过");

    for id in [RECENT_ID, HISTORY_ID] {
        runtime.set_enabled(id, false).expect("停用");
    }
    for id in [RECENT_ID, HISTORY_ID] {
        let status = runtime.list().into_iter().find(|plugin| plugin.id == id).map(|p| p.status);
        assert_eq!(status, Some(PluginStatus::Stopped), "{id} 停用后应回到 Stopped");
    }
    println!("停用：通过");

    runtime.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// 宿主侧回执：主题 / 量算 / 最近打开清单 / 历史分组（首次失败一次）/ 打开请求记录。
#[derive(Clone)]
struct HostState {
    recent_entries: Arc<Mutex<Vec<RecentEntry>>>,
    opens: Arc<Mutex<Vec<(String, String)>>>,
    /// `editor.openFile` 的 reveal 标记（列表类入口应为 false）。
    open_reveals: Arc<Mutex<Vec<bool>>>,
    history_refreshes: Arc<Mutex<Vec<bool>>>,
    history_calls: Arc<AtomicUsize>,
    history_fail_once: Arc<AtomicBool>,
}

impl HostState {
    fn new() -> Self {
        Self {
            recent_entries: Arc::new(Mutex::new(Vec::new())),
            opens: Arc::new(Mutex::new(Vec::new())),
            open_reveals: Arc::new(Mutex::new(Vec::new())),
            history_refreshes: Arc::new(Mutex::new(Vec::new())),
            history_calls: Arc::new(AtomicUsize::new(0)),
            history_fail_once: Arc::new(AtomicBool::new(false)),
        }
    }
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
                        .map(|item| MeasureTextResult { width: item.text.chars().count() as f32 * item.size * 0.55, height: item.size * 1.2, lines: 1, segments: None })
                        .collect();
                    reply(&runtime, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::RecentList { request_id, .. } => {
                    let entries = state.recent_entries.lock().unwrap().clone();
                    let result = RecentListResult { ready: true, entries };
                    reply(&runtime, request_id, serde_json::to_value(result).unwrap());
                }
                HostRequest::HistoryGroups { request_id, params, .. } => {
                    state.history_calls.fetch_add(1, Ordering::SeqCst);
                    state.history_refreshes.lock().unwrap().push(params.refresh);
                    if state.history_fail_once.swap(false, Ordering::SeqCst) {
                        runtime.resolve_shell_call(
                            request_id,
                            Err("冒烟：首次聚合失败".to_string()),
                        );
                    } else {
                        reply(
                            &runtime,
                            request_id,
                            json!({
                                "ready": true,
                                "groups": [
                                    { "kind": "note", "file": "notes/方案.md", "count": 3 },
                                    { "kind": "table", "file": "plan.atb", "count": 1 }
                                ]
                            }),
                        );
                    }
                }
                HostRequest::EditorOpenFile { request_id, path, reveal, .. } => {
                    state.opens.lock().unwrap().push(("openFile".into(), path));
                    state.open_reveals.lock().unwrap().push(reveal);
                    reply(&runtime, request_id, json!({}));
                }
                HostRequest::HistoryOpen { request_id, params, .. } => {
                    state
                        .opens
                        .lock()
                        .unwrap()
                        .push(("historyOpen".into(), format!("{}:{}", params.kind, params.file)));
                    reply(&runtime, request_id, json!({}));
                }
                other => eprintln!("未处理的宿主请求：{other:?}"),
            }
        }
    });
}

fn recent(file: &str, kind: RecentKind, opened_at: i64) -> RecentEntry {
    RecentEntry { file: file.into(), kind, opened_at }
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

/// 按下 + 抬起一条命中区（宿主判定双击 / 点击的同一路径）。
fn click(runtime: &PluginRuntime, plugin: &str, surface: &str, frame: &SurfaceFrame, hit_id: &str) {
    let hit = frame.hits.iter().find(|hit| hit.id == hit_id).expect("命中区应存在");
    let (x, y) = (hit.x + hit.w / 2.0, hit.y + hit.h / 2.0);
    for input in [
        FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: Some(hit_id.to_string()) },
        FrameInput::PointerUp { x, y, button: MouseButton::Left, hit: Some(hit_id.to_string()) },
    ] {
        runtime.notify_frame_input(plugin, surface, &input);
    }
}

/// 取某一行卡片上的命中区 id（`open:<世代>:<行号>`）：行号按渲染那一帧算。
fn open_hit(frame: &SurfaceFrame, index: usize) -> String {
    frame
        .hits
        .iter()
        .find(|hit| hit.id.ends_with(&format!(":{index}")))
        .expect("应有可点卡片")
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
    plugin: &str,
    surface: &str,
    accept: impl Fn(&SurfaceFrame) -> bool,
) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(frame) = runtime.surface_frame(plugin, surface) {
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
            background: "#131418".into(),
            border_subtle: "#252a31".into(),
            selection: "#e0a94e40".into(),
            scrim: "#00000099".into(),
            transparent: "#00000000".into(),
        },
        font_sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
        radii: Radii { xs: 4.0, sm: 6.0, md: 8.0 },
        event_colors: vec!["#e06c75".into(), "#61afef".into(), "#98c379".into()],
    }
}
