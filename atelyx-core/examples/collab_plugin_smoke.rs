//! 协作房间插件无界面冒烟：经真实 `PluginRuntime` 加载、挂面、取帧，覆盖本地仓库空态、
//! 空间名册与在线设备（含同身份合并）、两个宿主入口、打开对方文件、断开态与停用。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, MouseButton, PluginRuntime, PluginStatus,
    SpaceContainerKind, SpaceMember, SpaceMemberRole, SpaceMembersResult, SpacePeer, SpacePeerFile,
    SpaceRoomResult, SpaceSelfIdentity, SurfaceKind,
};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};
use atelyx_plugin_protocol::paint::DrawCommand;

const COLLAB_ID: &str = "atelyx.collab";
const COLLAB_SURFACE: &str = "collabroom";
/// 冒烟下发的面板尺寸（与 SDK 首帧占位尺寸不同，才能证明按尺寸重排生效）。
const PANEL_W: f32 = 900.0;
const PANEL_H: f32 = 600.0;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    assert_eq!(
        format!("plugin:{COLLAB_ID}:{COLLAB_SURFACE}"),
        atelyx_core::layout::COLLAB_VIEW
    );
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    // 在临时副本上跑：停用会真删插件目录
    let root = std::env::temp_dir().join(format!("atelyx-collab-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.clone(), "0.6.5");
    runtime.scan();

    let state = HostState::new();
    serve(runtime.clone(), state.clone());

    enable(&runtime, COLLAB_ID).await;
    wait_running(&runtime, COLLAB_ID).await;
    runtime.notify_surface_attached(COLLAB_ID, COLLAB_SURFACE, SurfaceKind::Panel);
    resize(&runtime, COLLAB_ID, COLLAB_SURFACE);

    // 本地仓库：空态 + 「协作空间」入口
    let local = wait_frame(&runtime, |frame| has_text(frame, "未进入协作空间"))
        .await
        .expect("本地仓库应出空态帧");
    let open_menu = "space-open-menu".to_string();
    click_hit(&runtime, &local, &open_menu);
    wait_count(&state.menus, 1, "宿主未收到「协作空间」菜单请求").await;
    println!("本地空态与菜单入口：通过");

    // 进入空间：名册 + 在线设备（两条同身份连接合并成一行，台数按连接数算）
    *state.room.lock().unwrap() = space_room();
    *state.members.lock().unwrap() = members();
    runtime.on_space_peers_changed();
    runtime.on_space_members_changed();
    let frame = wait_frame(&runtime, |frame| {
        has_text(frame, "已连接") && has_text(frame, "3 台在线") && has_text(frame, "所有者")
    })
    .await
    .expect("空间快照到达后应出房间帧");
    assert!(has_text(&frame, "2 人"), "名册人数按成员数");
    assert!(has_text(&frame, "只读"), "角色徽标");
    assert!(has_text(&frame, "@guest"));
    assert!(has_text(&frame, "我"), "本端设备行标「我」");
    assert!(has_text(&frame, "Linux · 0.6.5"), "对端平台与版本");
    assert!(has_text(&frame, "plan.atb") && has_text(&frame, "c.md"), "合并后文件并集");
    println!("空间名册与设备：通过");

    // 两个宿主入口
    click_hit(&runtime, &frame, "space-settings");
    wait_count(&state.settings, 1, "宿主未收到「协作设置」请求").await;

    // 打开对端设备上的第一行文件（设备行 1、文件行 0；设备行 0 是本端）
    let file_hit = frame
        .hits
        .iter()
        .find(|hit| hit.id.starts_with("file:") && hit.id.ends_with(":1:0"))
        .expect("应有可点的打开文件行")
        .id
        .clone();
    click_hit(&runtime, &frame, &file_hit);
    wait_opens(&state, 1).await;
    assert_eq!(state.opens.lock().unwrap()[0], "a.md");
    println!("协作设置与打开文件：通过");

    // 名册加载中：只给等待文案，不画成员卡片
    *state.members.lock().unwrap() = SpaceMembersResult {
        ready: false,
        loading: true,
        error: None,
        members: Vec::new(),
    };
    runtime.on_space_members_changed();
    let loading = wait_frame(&runtime, |frame| has_text(frame, "加载中…"))
        .await
        .expect("名册加载中应出等待文案");
    assert!(!has_text(&loading, "所有者"));
    println!("名册加载中：通过");

    // 名册失败：错误优先于缓存内容（宿主会同时保留上次成功的名册）
    *state.members.lock().unwrap() = SpaceMembersResult {
        ready: true,
        loading: false,
        error: Some("网络不可达".into()),
        members: members().members,
    };
    runtime.on_space_members_changed();
    let failed = wait_frame(&runtime, |frame| has_text(frame, "成员名册加载失败：网络不可达"))
        .await
        .expect("名册失败应出可读原因");
    assert!(!has_text(&failed, "所有者"), "失败时不画成员卡片（错误优先于缓存）");
    println!("名册失败态：通过");

    // 断开：状态行与空提示都回到未连接
    *state.members.lock().unwrap() = SpaceMembersResult::default();
    *state.room.lock().unwrap() = SpaceRoomResult {
        kind: SpaceContainerKind::Space,
        connected: false,
        self_peer_id: None,
        self_identity: Some(self_identity()),
        peers: Vec::new(),
    };
    runtime.on_space_peers_changed();
    runtime.on_space_members_changed();
    let offline = wait_frame(&runtime, |frame| {
        has_text(frame, "未连接") && has_text(frame, "连接中…") && has_text(frame, "1 台在线")
    })
    .await
    .expect("断开后应出未连接帧");
    assert!(!has_text(&offline, "当前空间没有其他设备在线"));
    println!("断开态：通过");

    runtime.set_enabled(COLLAB_ID, false).expect("停用");
    let status = runtime
        .list()
        .into_iter()
        .find(|plugin| plugin.id == COLLAB_ID)
        .map(|plugin| plugin.status);
    assert_eq!(status, Some(PluginStatus::Stopped), "停用后应回到 Stopped");
    println!("停用：通过");

    runtime.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// 宿主侧回执：主题 / 量算 / 房间与名册快照 / 两个入口与打开请求的记录。
#[derive(Clone)]
struct HostState {
    room: Arc<Mutex<SpaceRoomResult>>,
    members: Arc<Mutex<SpaceMembersResult>>,
    opens: Arc<Mutex<Vec<String>>>,
    settings: Arc<Mutex<usize>>,
    menus: Arc<Mutex<usize>>,
}

impl HostState {
    fn new() -> Self {
        Self {
            room: Arc::new(Mutex::new(SpaceRoomResult::default())),
            members: Arc::new(Mutex::new(SpaceMembersResult::default())),
            opens: Arc::new(Mutex::new(Vec::new())),
            settings: Arc::new(Mutex::new(0)),
            menus: Arc::new(Mutex::new(0)),
        }
    }
}

fn file(path: &str, icon: &str) -> SpacePeerFile {
    SpacePeerFile { path: path.into(), icon: icon.into() }
}

fn self_identity() -> SpaceSelfIdentity {
    SpaceSelfIdentity {
        name: "小明".into(),
        color: "#30bced".into(),
        platform: "windows".into(),
        version: "0.6.5".into(),
        files: vec![file("notes/方案.md", "sticky-note")],
    }
}

fn peer(id: &str, files: Vec<SpacePeerFile>) -> SpacePeer {
    SpacePeer {
        peer_id: id.into(),
        nickname: "小红".into(),
        device: "桌面端".into(),
        color: "#e06c75".into(),
        platform: Some("linux".into()),
        version: Some("0.6.5".into()),
        files,
    }
}

fn space_room() -> SpaceRoomResult {
    SpaceRoomResult {
        kind: SpaceContainerKind::Space,
        connected: true,
        self_peer_id: Some("1".into()),
        self_identity: Some(self_identity()),
        peers: vec![
            peer("2", vec![file("a.md", "sticky-note"), file("plan.atb", "table")]),
            peer("3", vec![file("docs/c.md", "sticky-note")]),
        ],
    }
}

fn members() -> SpaceMembersResult {
    SpaceMembersResult {
        ready: true,
        loading: false,
        error: None,
        members: vec![
            SpaceMember {
                username: "owner".into(),
                display_name: "所有者".into(),
                role: SpaceMemberRole::Owner,
            },
            SpaceMember {
                username: "guest".into(),
                display_name: "访客".into(),
                role: SpaceMemberRole::Viewer,
            },
        ],
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
                HostRequest::SpaceRoom { request_id, .. } => {
                    let room = state.room.lock().unwrap().clone();
                    reply(&runtime, request_id, serde_json::to_value(room).unwrap());
                }
                HostRequest::SpaceMembers { request_id, .. } => {
                    let members = state.members.lock().unwrap().clone();
                    reply(&runtime, request_id, serde_json::to_value(members).unwrap());
                }
                HostRequest::SpaceOpenSettings { request_id, .. } => {
                    *state.settings.lock().unwrap() += 1;
                    reply(&runtime, request_id, json!({}));
                }
                HostRequest::SpaceOpenMenu { request_id, .. } => {
                    *state.menus.lock().unwrap() += 1;
                    reply(&runtime, request_id, json!({}));
                }
                HostRequest::EditorOpenFile { request_id, path, .. } => {
                    state.opens.lock().unwrap().push(path);
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
        runtime.notify_frame_input(COLLAB_ID, COLLAB_SURFACE, &input);
    }
}

async fn wait_opens(state: &HostState, wanted: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.opens.lock().unwrap().len() < wanted {
        assert!(Instant::now() < deadline, "宿主未收到打开文件请求");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_count(counter: &Arc<Mutex<usize>>, wanted: usize, reason: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while *counter.lock().unwrap() < wanted {
        assert!(Instant::now() < deadline, "{reason}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn reply(runtime: &PluginRuntime, request_id: u64, result: Value) {
    runtime.resolve_shell_call(request_id, Ok(result));
}

/// 帧里是否画过这段文字（状态行 / 成员名 / 文件名断言用）。
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
        if let Some(frame) = runtime.surface_frame(COLLAB_ID, COLLAB_SURFACE) {
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
