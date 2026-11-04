//! 同进程日历插件的无界面冒烟：经真实 `PluginRuntime` 加载、挂面、收帧、点击、停用。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, MouseButton, PluginRuntime, PluginStatus,
    SurfaceKind,
};
use atelyx_plugin_protocol::event::{FileChangeKind, VaultSwitchPayload};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::paint::DrawCommand;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};

const ID: &str = "com.atelyx.calendar";
const SURFACE: &str = "calendar";
/// 本地日程文件（与插件的落点一致）。
const CALENDAR_FILE: &str = ".atelyx/calendar.json";
/// 损坏备份名的前缀。
const BACKUP_PREFIX: &str = ".atelyx/calendar.json.corrupt-";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    // 在临时副本上跑：卸载会真删插件目录（也验证 Windows 上动态库能随实例释放）
    let root = atelyx_core::scratch::TempDir::new("abi-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
    runtime.scan();

    let vault = Arc::new(Mutex::new(HashMap::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let reads = Arc::new(Mutex::new(Vec::new()));
    let responder = runtime.clone();
    let files = vault.clone();
    let written = writes.clone();
    let read = reads.clone();
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
                        .map(|item| MeasureTextResult { width: item.text.chars().count() as f32 * item.size * 0.55, height: item.size * 1.2, lines: 1, segments: None })
                        .collect();
                    reply(&responder, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::HistoryActivity { request_id, .. } => {
                    reply(&responder, request_id, json!({ "days": [] }));
                }
                HostRequest::VaultExists { request_id, path, .. } => {
                    let exists = files.lock().unwrap().contains_key(&path);
                    reply(&responder, request_id, json!({ "exists": exists }));
                }
                HostRequest::VaultReadText { request_id, path, .. } => {
                    read.lock().unwrap().push(path.clone());
                    match files.lock().unwrap().get(&path).cloned() {
                        Some(content) => reply(&responder, request_id, json!({ "content": content })),
                        None => responder.resolve_shell_call(request_id, Err("文件不存在".into())),
                    }
                }
                HostRequest::VaultWriteText { request_id, path, content, .. } => {
                    written.lock().unwrap().push((path.clone(), content.clone()));
                    files.lock().unwrap().insert(path, content);
                    reply(&responder, request_id, json!({}));
                }
                HostRequest::VaultRemove { request_id, path, .. } => {
                    files.lock().unwrap().remove(&path);
                    reply(&responder, request_id, json!({}));
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

    // 损坏文件：原文保留、备份一份、报错可见；重复读取不重复备份
    let truncated = r#"{"schema":"atelyx-calendar/v1","items":[{"id":"a","date":"2026-10-01","tit"#;
    vault.lock().unwrap().insert(CALENDAR_FILE.to_string(), truncated.to_string());

    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "smoke".into() });
    runtime.notify_surface_attached(ID, SURFACE, SurfaceKind::Panel);
    let frame = wait_frame(&runtime, Duration::from_secs(3)).await.expect("attach 后应有帧");
    assert!(!frame.hits.is_empty(), "日历帧应有命中区");
    assert!(frame_has_text(&frame, "日程文件损坏"), "损坏应给出可见错误");
    {
        let files = vault.lock().unwrap();
        assert_eq!(
            files.get(CALENDAR_FILE).map(String::as_str),
            Some(truncated),
            "损坏原文不得被删除或改写"
        );
        let backups = backup_names(&files);
        assert_eq!(backups.len(), 1, "损坏原文应留一份备份：{backups:?}");
        assert_eq!(files[&backups[0]], truncated, "备份内容应是原文");
    }
    println!("损坏文件：原文保留 + 备份：通过");

    // 重复读取：同样的损坏内容不再写第二份备份（同一容器再切不会触发重读，用文件变更事件）
    runtime.on_file_changed(CALENDAR_FILE, FileChangeKind::Updated, None);
    wait_reads(&reads, CALENDAR_FILE, 2).await;
    assert_eq!(backup_names(&vault.lock().unwrap()).len(), 1, "重复读取不得堆积备份");
    println!("重复读取：单份备份：通过");

    // 空文件 = 可能同步未完成：报错但不删不写
    vault.lock().unwrap().insert(CALENDAR_FILE.to_string(), String::new());
    runtime.on_file_changed(CALENDAR_FILE, FileChangeKind::Updated, None);
    let frame = wait_text(&runtime, "同步未完成").await;
    assert!(
        vault.lock().unwrap().contains_key(CALENDAR_FILE),
        "空文件不得被删除"
    );
    assert!(
        !writes.lock().unwrap().iter().any(|(path, _)| path == CALENDAR_FILE),
        "读侧异常时不得写日程文件"
    );
    println!("空文件：报错不删不写：通过");

    // 带 BOM 的文件照常读出（否则会走损坏分支）
    let bom = format!(
        "\u{feff}{}",
        r#"{"schema":"atelyx-calendar/v1","items":[{"id":"b","date":"2026-10-01","title":"带 BOM 的日程"}]}"#
    );
    vault.lock().unwrap().insert(CALENDAR_FILE.to_string(), bom);
    let before = frame;
    runtime.on_file_changed(CALENDAR_FILE, FileChangeKind::Updated, None);
    wait_reads(&reads, CALENDAR_FILE, 4).await;
    let frame = wait_frame_after(&runtime, &before, Duration::from_secs(3))
        .await
        .expect("BOM 文件后应有新帧");
    assert!(
        !frame_has_text(&frame, "日程文件损坏") && !frame_has_text(&frame, "同步未完成"),
        "带 BOM 的日程文件应正常读出"
    );
    println!("BOM 文件：通过");

    // 收尾：换成空清单文件，后续按空仓库继续（帧可能与上一步一致，按读取次数同步）
    vault
        .lock()
        .unwrap()
        .insert(CALENDAR_FILE.to_string(), r#"{"schema":"atelyx-calendar/v1","items":[]}"#.to_string());
    runtime.on_file_changed(CALENDAR_FILE, FileChangeKind::Updated, None);
    wait_reads(&reads, CALENDAR_FILE, 5).await;
    // 空清单与上一步 BOM 帧布局相同，翻月按钮命中区沿用该帧
    println!("空清单帧：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());

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

/// 等到某个文件被读够 want 次：读侧异常时的同步点（帧内容可能一模一样）。
async fn wait_reads(reads: &Arc<Mutex<Vec<String>>>, path: &str, want: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let count = reads.lock().unwrap().iter().filter(|item| *item == path).count();
        if count >= want {
            return;
        }
        if Instant::now() >= deadline {
            panic!("{path} 读取次数未达到 {want}：{count}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 等到帧里出现某段文字。
async fn wait_text(runtime: &PluginRuntime, needle: &str) -> SurfaceFrame {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(frame) = runtime.surface_frame(ID, SURFACE) {
            if frame_has_text(&frame, needle) {
                return (*frame).clone();
            }
        }
        if Instant::now() >= deadline {
            panic!("帧里未出现「{needle}」");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 帧里是否画出了某段文字（错误提示这类断言用）。
fn frame_has_text(frame: &SurfaceFrame, needle: &str) -> bool {
    frame.commands.iter().any(|command| match command {
        DrawCommand::Text { content, .. } => content.contains(needle),
        _ => false,
    })
}

/// 当前损坏备份的路径（按名排序，便于比较数量）。
fn backup_names(files: &HashMap<String, String>) -> Vec<String> {
    let mut names: Vec<String> = files
        .keys()
        .filter(|path| path.starts_with(BACKUP_PREFIX))
        .cloned()
        .collect();
    names.sort();
    names
}
