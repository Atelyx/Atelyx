//! 笔记预置插件的无界面冒烟：装载、挂面、打开文档、三档切换与块级编辑走一遍。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, PluginRuntime, PluginStatus, SurfaceKind,
};
use atelyx_plugin_protocol::frame::{Key, KeyAction, Modifiers, NamedKey, SurfaceFrame};
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};
use atelyx_plugin_protocol::paint::{DrawCommand, TextAlign};

const PLUGIN: &str = "atelyx.notes";
const SURFACE: &str = "note";
const NOTE: &str = "---\ntags: [开发]\n---\n# 标题\n\n第一段正文。\n\n- 列表项一\n- 列表项二\n\n1. 步骤一\n2. 步骤二\n";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = atelyx_core::scratch::TempDir::new("notes-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
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
                            .map(|item| MeasureTextResult { width: item.text.chars().map(|ch| item.size * if (ch as u32) < 0x1100 { 0.55 } else { 1.0 }).sum(), height: item.size * 1.2, lines: 1, segments: None })
                            .collect();
                        reply(&responder, request_id, json!({ "sizes": sizes }));
                    }
                    HostRequest::DocOpen { request_id, params, .. } => {
                        reply(
                            &responder,
                            request_id,
                            json!({ "path": params.path, "text": NOTE, "canWrite": true }),
                        );
                    }
                    HostRequest::VaultBacklinks { request_id, .. } => {
                        reply(&responder, request_id, json!({ "entries": [] }));
                    }
                    HostRequest::VaultTags { request_id, .. } => {
                        reply(&responder, request_id, json!({ "tags": ["开发", "设计"] }));
                    }
                    HostRequest::EditorPrefs { request_id, .. } => {
                        reply(
                            &responder,
                            request_id,
                            json!({ "softWrap": true, "inPageTitle": true, "contentWidth": 780 }),
                        );
                    }
                    HostRequest::DocRename { request_id, .. } => {
                        reply(&responder, request_id, json!({}));
                    }
                    HostRequest::DocEdit { request_id, .. }
                    | HostRequest::DocAwareness { request_id, .. } => {
                        reply(&responder, request_id, json!({}));
                    }
                    HostRequest::DocSave { request_id, .. } => {
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
    assert_empty_state_centered(
        &frame,
        "打开笔记",
        "在文件面板或搜索面板单击一个 .md 笔记开始编辑。",
    );

    runtime.notify_doc_open(PLUGIN, SURFACE, "a.md");
    let frame = wait_frame(&runtime, Duration::from_secs(5)).await.expect("打开笔记后应有帧");
    println!("只读帧：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());

    if let Some(hit) = frame.hits.iter().find(|hit| hit.id == "note:mode:edit").cloned() {
        click_hit(&runtime, &hit);
        let frame = wait_hit(&runtime, "note:gap:1:0", Duration::from_secs(2))
            .await
            .expect("切换预览编辑档后应有空行输入面");
        let blocks = frame.hits.iter().filter(|hit| hit.id.starts_with("note:block:")).count();
        let gaps = frame.hits.iter().filter(|hit| hit.id.starts_with("note:gap:")).count();
        let first_y = frame.hits.iter().find(|hit| hit.id == "note:block:0").map_or(-1.0, |hit| hit.y);
        println!("预览编辑块：{blocks}，空行输入面：{gaps}，首块 y：{first_y:.1}");
        assert!(first_y < 200.0, "工具条 / 属性面板不得吃满剩余高度（首块 y = {first_y}）");
        // 揭示块：块后一根空行应显示为第二行（修复前会多算一行）
        if let Some(hit) = frame.hits.iter().find(|hit| hit.id == "note:block:0").cloned() {
            click_hit(&runtime, &hit);
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut revealed = None;
            while Instant::now() < deadline {
                if let Some(frame) = runtime.surface_frame(PLUGIN, SURFACE) {
                    if let Some(hit) = frame.hits.iter().find(|hit| hit.id == "note:block:0") {
                        if hit.h > 32.0 {
                            revealed = Some(frame);
                            break;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let frame = revealed.expect("揭示标题");
            let height = frame.hits.iter().find(|hit| hit.id == "note:block:0").map_or(0.0, |hit| hit.h);
            println!("标题揭示高度（块后一根空行）：{height:.1}");
            assert!((32.0..48.0).contains(&height), "标题 + 一根空行应为两行（高度 {height}）");
            // 清掉活动块，块间空行输入面才会重新出现
            if let Some(hit) = frame.hits.iter().find(|hit| hit.id == "note:block:1").cloned() {
                click_hit(&runtime, &hit);
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
        }
        let frame = runtime.surface_frame(PLUGIN, SURFACE).expect("预览编辑帧");
        if let Some(hit) = frame.hits.iter().find(|hit| hit.id.starts_with("note:gap:")).cloned() {
            click_hit(&runtime, &hit);
            runtime.notify_frame_input(
                PLUGIN,
                SURFACE,
                &FrameInput::Text {
                    text: "空行".into(),
                    composing: false,
                    caret: None,
                    candidates: None,
                },
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
            let inserted = runtime.surface_frame(PLUGIN, SURFACE).is_some_and(|frame| {
                frame
                    .commands
                    .iter()
                    .any(|command| matches!(command, atelyx_plugin_protocol::paint::DrawCommand::Text { content, .. } if content == "空行"))
            });
            println!("空行就地输入：{}", if inserted { "通过" } else { "未确认" });
        }
        // 列表项逐项揭示：点第二项只把该项换成源码，另一项保持渲染
        let frame = runtime.surface_frame(PLUGIN, SURFACE).expect("预览编辑帧");
        let items: Vec<String> = frame
            .hits
            .iter()
            .filter(|hit| hit.id.starts_with("note:item:"))
            .map(|hit| hit.id.clone())
            .collect();
        let list_blocks = frame
            .hits
            .iter()
            .filter(|hit| hit.id == "note:block:2" || hit.id == "note:block:3")
            .count();
        println!("列表项命中：{items:?}，列表块命中：{list_blocks}");
        assert_eq!(items.len(), 4, "两个列表各两项都要有独立命中区：{items:?}");
        assert_eq!(list_blocks, 0, "列表块不再整块命中");
        if let Some(hit) = frame.hits.iter().find(|hit| hit.id == "note:item:3:1").cloned() {
            click_hit(&runtime, &hit);
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut revealed = None;
            while Instant::now() < deadline {
                if let Some(frame) = runtime.surface_frame(PLUGIN, SURFACE) {
                    if frame.commands.iter().any(|command| {
                        matches!(command, atelyx_plugin_protocol::paint::DrawCommand::Text { content, .. } if content == "2. 步骤二")
                    }) {
                        revealed = Some(frame);
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let frame = revealed.expect("揭示有序列表第二项");
            let first_still_rendered = frame.commands.iter().any(|command| {
                matches!(command, atelyx_plugin_protocol::paint::DrawCommand::Text { content, .. } if content == "1.")
            });
            let height = frame.hits.iter().find(|hit| hit.id == "note:item:3:1").map_or(0.0, |hit| hit.h);
            println!("有序项揭示：首项渲染 {first_still_rendered}，高度 {height:.1}");
            assert!(first_still_rendered, "另一项要保持渲染");
            assert!(height < 32.0, "揭示项不得多出一行（高度 {height}）");
        }
    }
    if let Some(hit) = runtime
        .surface_frame(PLUGIN, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "props:add").cloned())
    {
        click_hit(&runtime, &hit);
        // 清掉占位键草稿（全选后删除）：候选池变为全部常用键，候选行才可命中
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        runtime.notify_frame_input(
            PLUGIN,
            SURFACE,
            &FrameInput::Key {
                key: Key::Character { text: "a".into() },
                action: KeyAction::Press,
                modifiers: ctrl,
                text: None,
            },
        );
        runtime.notify_frame_input(
            PLUGIN,
            SURFACE,
            &FrameInput::Key {
                key: Key::Named { name: NamedKey::Backspace },
                action: KeyAction::Press,
                modifiers: Modifiers::default(),
                text: None,
            },
        );
        let frame = wait_hit(&runtime, "props:suggest:0", Duration::from_secs(2)).await;
        println!("属性键名候选卡：{}", if frame.is_some() { "通过" } else { "未确认" });
        if let Some(hit) = frame
            .and_then(|frame| frame.hits.iter().find(|hit| hit.id.starts_with("props:item:0:")).cloned())
        {
            click_hit(&runtime, &hit);
            // 等到标签槽进入编辑态（该胶囊命中消失、候选行出现）再断言，避免读到上一帧
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut tagged = None;
            while Instant::now() < deadline {
                if let Some(frame) = runtime.surface_frame(PLUGIN, SURFACE) {
                    if !frame.hits.iter().any(|hit| hit.id == "props:item:0:0") {
                        tagged = Some(frame);
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let ok = tagged.is_some_and(|frame| {
                frame.hits.iter().any(|hit| hit.id == "props:suggest:0")
                    && frame.commands.iter().any(|command| {
                        matches!(command, atelyx_plugin_protocol::paint::DrawCommand::Text { content, .. } if content == "#设计")
                    })
            });
            println!("标签候选（vault.tags）：{}", if ok { "通过" } else { "未确认" });
        }
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
        if let Some(frame) = runtime.surface_frame(PLUGIN, SURFACE) {
            let height = frame.hits.iter().find(|hit| hit.id == "note:block:0").map_or(0.0, |hit| hit.h);
            println!("揭示块高度：{height:.1}");
            assert!(height < 32.0, "揭示块不得多出一行（高度 {height}）");
        }
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
    runtime.shutdown();
    println!("停用：通过");
    if let Ok(log) = std::fs::read_to_string(root.join(PLUGIN).join("logs").join("runtime.log")) {
        println!("插件日志：\n{log}");
    }
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
            background: "#131418".into(),
            border_subtle: "#252a31".into(),
            selection: "#e0a94e40".into(),
            scrim: "#00000099".into(),
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
