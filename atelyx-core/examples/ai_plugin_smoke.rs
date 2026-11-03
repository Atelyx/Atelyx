//! AI 预置插件的无界面冒烟：装载 ai-core / ai-panel，挂面取帧，再停用 / 卸载。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, PluginRuntime, PluginStatus, SurfaceKind,
};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};

const PANEL: &str = "atelyx.ai-panel";
const CORE: &str = "atelyx.ai-core";
const SURFACE: &str = "aichat";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = std::env::temp_dir().join(format!("atelyx-ai-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.clone(), "0.6.5");
    runtime.scan();

    // 宿主请求应答放在独立线程：改宽基准会忙等，不能占住应答任务所在的运行时
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
                        // 粗略量算：ASCII 按 0.55 字宽，其余（CJK / 全角）按一字宽
                        let sizes: Vec<MeasureTextResult> = params
                            .items
                            .iter()
                            .map(|item| MeasureTextResult { width: item
                                    .text
                                    .chars()
                                    .map(|ch| {
                                        if (ch as u32) < 0x1100 {
                                            item.size * 0.55
                                        } else {
                                            item.size
                                        }
                                    })
                                    .sum(), height: item.size * 1.2, lines: 1, segments: None })
                            .collect();
                        reply(&responder, request_id, json!({ "sizes": sizes }));
                    }
                    HostRequest::AiModels { request_id, .. } => {
                        reply(&responder, request_id, json!({ "providers": [] }));
                    }
                    HostRequest::AiAgents { request_id, .. } => {
                        reply(
                            &responder,
                            request_id,
                            json!({
                                "agents": [
                                    { "id": "chat", "name": "对话", "builtin": true },
                                    { "id": "agent", "name": "Agent", "builtin": true },
                                    { "id": "review", "name": "审阅", "builtin": false },
                                    { "id": "archive", "name": "归档", "builtin": false }
                                ]
                            }),
                        );
                    }
                    HostRequest::ChatHistory { request_id, params, .. } => {
                        if params.session_id.is_some() {
                            reply(
                                &responder,
                                request_id,
                                json!({
                                    "sessions": [],
                                    "session": {
                                        "id": "s1",
                                        "title": "长会话",
                                        "createdAt": 1,
                                        "updatedAt": 2,
                                        "messages": long_messages(),
                                    }
                                }),
                            );
                        } else {
                            reply(
                                &responder,
                                request_id,
                                json!({
                                    "sessions": [{
                                        "id": "s1",
                                        "title": "长会话",
                                        "updatedAt": 2,
                                        "messageCount": 100
                                    }]
                                }),
                            );
                        }
                    }
                    HostRequest::ChatSessionRead { request_id, .. } => {
                        reply(
                            &responder,
                            request_id,
                            json!({
                                "id": "s1",
                                "title": "长会话",
                                "createdAt": 1,
                                "updatedAt": 2,
                                "messages": long_messages(),
                            }),
                        );
                    }
                    HostRequest::ChatSessionSnapshot { request_id, .. } => {
                        reply(
                            &responder,
                            request_id,
                            json!({ "sessionVaultKey": "smoke", "sessions": [] }),
                        );
                    }
                    HostRequest::VaultList { request_id, path, .. } => {
                        let entries = if path.is_empty() {
                            json!([
                                { "name": "方案.md", "kind": "file" },
                                { "name": "资料", "kind": "dir" }
                            ])
                        } else {
                            json!([{ "name": "看板.atb", "kind": "file" }])
                        };
                        reply(&responder, request_id, json!({ "entries": entries }));
                    }
                    // 打开文件：把「资料/看板.atb」报成打开中，候选里它应排到最前
                    HostRequest::EditorOpenFiles { request_id, .. } => {
                        reply(&responder, request_id, json!({ "paths": ["资料/看板.atb"] }));
                    }
                    HostRequest::VaultExists { request_id, .. } => {
                        reply(&responder, request_id, json!({ "exists": false }));
                    }
                    other => eprintln!("未处理的宿主请求：{other:?}"),
                }
            }
        });
    });

    for id in [CORE, PANEL] {
        let runtime = runtime.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || runtime.set_enabled(&id, true))
            .await
            .expect("启用任务不应 panic")
            .expect("启用插件");
    }
    wait_status(&runtime, PANEL, PluginStatus::Running).await;
    wait_status(&runtime, CORE, PluginStatus::Running).await;

    runtime.notify_surface_attached(PANEL, SURFACE, SurfaceKind::Panel);
    // 插件拿到真实尺寸才出首帧（与宿主首绘同步）；这里替宿主补一次尺寸
    runtime.notify_frame_input(
        PANEL,
        SURFACE,
        &FrameInput::Resize { width: 900.0, height: 700.0 },
    );
    let frame = wait_frame(&runtime, Duration::from_secs(5)).await.expect("attach 后应有帧");
    println!("面板首帧：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());
    let frame = runtime.surface_frame(PANEL, SURFACE).expect("初始帧");
    if let Ok(json) = serde_json::to_vec(&*frame) {
        let _ = std::fs::write("target/ai_frame_empty.json", json);
    }
    // 打开侧栏，再选中最长的会话，让改宽基准覆盖真实消息流的排版成本
    if let Some(hit) = frame.hits.iter().find(|hit| hit.id == "toggle-sidebar") {
        click_hit(&runtime, hit);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let frame = runtime.surface_frame(PANEL, SURFACE).expect("侧栏帧");
    if let Some(hit) = frame.hits.iter().find(|hit| hit.id.starts_with("session:")) {
        click_hit(&runtime, hit);
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(frame) = runtime.surface_frame(PANEL, SURFACE) {
            println!("选中会话后：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());
        }
    }
    // 回滚二段确认：确认态与图标钮必须是不同命中 id，否则整帧被宿主拒收
    if let Some(hit) = runtime
        .surface_frame(PANEL, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id.starts_with("rollback:")).cloned())
    {
        click_hit(&runtime, &hit);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let confirmed = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
            frame
                .hits
                .iter()
                .any(|hit| hit.id.starts_with("rollback-confirm:"))
        });
        println!("回滚确认帧：{}", if confirmed { "通过" } else { "缺失" });
        if let Some(cancel) = runtime
            .surface_frame(PANEL, SURFACE)
            .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "rollback-cancel").cloned())
        {
            click_hit(&runtime, &cancel);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    // 上翻消息流后应出现「回到底部」，点击后恢复贴底
    runtime.notify_frame_input(
        PANEL,
        SURFACE,
        &FrameInput::Wheel {
            x: 260.0,
            y: 300.0,
            delta_x: 0.0,
            delta_y: -400.0,
            modifiers: Default::default(), hit: None,
        },
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let scrolled_up = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
        frame.hits.iter().any(|hit| hit.id == "scroll-bottom")
    });
    println!("回到底部浮钮：{}", if scrolled_up { "出现" } else { "缺失" });
    if scrolled_up {
        if let Some(frame) = runtime.surface_frame(PANEL, SURFACE) {
            if let Ok(json) = serde_json::to_vec(&*frame) {
                let _ = std::fs::write("target/ai_frame_scrolled.json", json);
            }
        }
        // 浮钮是浮层但不能拦滚轮：再滚一次，内容应继续上移
        let before = top_message_y(&runtime);
        runtime.notify_frame_input(
            PANEL,
            SURFACE,
            &FrameInput::Wheel {
                x: 260.0,
                y: 300.0,
                delta_x: 0.0,
                delta_y: -400.0,
                modifiers: Default::default(), hit: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = top_message_y(&runtime);
        println!(
            "浮层上方继续滚轮：{}",
            if matches!((before, after), (Some(b), Some(a)) if a < b) {
                "通过"
            } else {
                "未移动"
            }
        );
        if let Some(hit) = runtime
            .surface_frame(PANEL, SURFACE)
            .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "scroll-bottom").cloned())
        {
            click_hit(&runtime, &hit);
            tokio::time::sleep(Duration::from_millis(100)).await;
            let back = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
                !frame.hits.iter().any(|hit| hit.id == "scroll-bottom")
            });
            println!("回到底部：{}", if back { "通过" } else { "未恢复" });
        }
    }
    // 窄面侧栏是覆盖层：点背板应收起，主区按钮仍可点（背板命中在主内容之后会被盖住）
    wait_resize(&runtime, 600.0, 700.0).expect("窄面尺寸");
    tokio::time::sleep(Duration::from_millis(100)).await;
    if let Some(hit) = runtime
        .surface_frame(PANEL, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "sidebar-backdrop").cloned())
    {
        click_hit(&runtime, &hit);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let closed = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
            !frame.hits.iter().any(|hit| hit.id == "sidebar-backdrop")
        });
        println!("窄面点背板收起侧栏：{}", if closed { "通过" } else { "未收起" });
    }
    // 重新打开侧栏，后续基准覆盖并排态
    if let Some(hit) = runtime
        .surface_frame(PANEL, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "toggle-sidebar").cloned())
    {
        click_hit(&runtime, &hit);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    resize_latency(&runtime, 900.0, 700.0).await;
    resize_latency(&runtime, 1200.0, 800.0).await;
    if let Some(frame) = runtime.surface_frame(PANEL, SURFACE) {
        if let Ok(json) = serde_json::to_vec(&*frame) {
            let _ = std::fs::write("target/ai_frame.json", json);
            println!("帧快照：target/ai_frame.json");
        }
    }
    // # 候选：聚焦输入框敲一个 #，候选贴输入盒内展开；键盘下移一行后应出现滚动位移
    if let Some(hit) = runtime
        .surface_frame(PANEL, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "draft").cloned())
    {
        click_hit(&runtime, &hit);
        tokio::time::sleep(Duration::from_millis(50)).await;
        runtime.notify_frame_input(
            PANEL,
            SURFACE,
            &FrameInput::Key {
                key: atelyx_plugin_protocol::frame::Key::Character { text: "#".into() },
                action: atelyx_plugin_protocol::frame::KeyAction::Press,
                modifiers: atelyx_plugin_protocol::frame::Modifiers::default(),
                text: Some("#".to_string()),
            },
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let first_pick = runtime.surface_frame(PANEL, SURFACE).and_then(|frame| {
            let picks = frame.hits.iter().filter(|hit| hit.id.starts_with("pick:")).count();
            // 候选顺序：打开中的文件置顶
            let order: Vec<String> = frame
                .hits
                .iter()
                .filter(|hit| hit.id.starts_with("pick:"))
                .map(|hit| hit.id.clone())
                .collect();
            println!("# 候选：{} 条（顺序 {order:?}）", picks);
            if let Ok(json) = serde_json::to_vec(&*frame) {
                let _ = std::fs::write("target/ai_frame_picker.json", json);
            }
            frame.hits.iter().find(|hit| hit.id == "pick:0").cloned()
        });
        // 点第一条候选：草稿落成完整标签，标签整段有浅强调底；点进标签整段选中、退格整段删除
        if let Some(pick) = first_pick {
            click_hit(&runtime, &pick);
            tokio::time::sleep(Duration::from_millis(100)).await;
            match runtime.surface_frame(PANEL, SURFACE) {
                Some(frame) => {
                    let tag = frame
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            atelyx_plugin_protocol::paint::DrawCommand::Text { content, .. }
                                if content.starts_with('#') =>
                            {
                                Some(content.clone())
                            }
                            _ => None,
                        });
                    let marked = frame.commands.iter().any(|command| {
                        matches!(
                            command,
                            atelyx_plugin_protocol::paint::DrawCommand::Rect {
                                fill: Some(atelyx_plugin_protocol::paint::PaintColor::Token {
                                    name: atelyx_plugin_protocol::paint::ColorToken::AccentSoft,
                                }),
                                ..
                            }
                        )
                    });
                    println!(
                        "引用标签：{}；标签高亮：{}",
                        tag.as_deref().unwrap_or("（未落成）"),
                        if marked { "有" } else { "无" }
                    );
                    if let Ok(json) = serde_json::to_vec(&*frame) {
                        let _ = std::fs::write("target/ai_frame_tag.json", json);
                    }
                    // 点进标签内部（取标签文字左下角往右一点）：整段选中后再退格应整段删掉
                    if let Some(text) = frame.commands.iter().find_map(|command| match command {
                        atelyx_plugin_protocol::paint::DrawCommand::Text { x, y, content, .. }
                            if content.starts_with('#') =>
                        {
                            Some((*x, *y, content.clone()))
                        }
                        _ => None,
                    }) {
                        let (x, y, content) = text;
                        let at = atelyx_plugin_protocol::frame::HitRegion {
                            id: "draft".into(),
                            x: x + 2.0,
                            y: y + 4.0,
                            w: 4.0,
                            h: 4.0,
                            cursor: None,
                        };
                        click_hit(&runtime, &at);
                        tokio::time::sleep(Duration::from_millis(60)).await;
                        for _ in 0..content.chars().count().max(1) {
                            runtime.notify_frame_input(
                                PANEL,
                                SURFACE,
                                &FrameInput::Key {
                                    key: atelyx_plugin_protocol::frame::Key::Named {
                                        name: atelyx_plugin_protocol::frame::NamedKey::Backspace,
                                    },
                                    action: atelyx_plugin_protocol::frame::KeyAction::Press,
                                    modifiers: atelyx_plugin_protocol::frame::Modifiers::default(),
                                    text: None,
                                },
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(80)).await;
                        let gone = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
                            !frame.commands.iter().any(|command| matches!(
                                command,
                                atelyx_plugin_protocol::paint::DrawCommand::Text { content, .. }
                                    if content.starts_with('#')
                            ))
                        });
                        println!("标签整段删除：{}", if gone { "通过" } else { "未删净" });
                    }
                }
                None => println!("引用标签：未取到帧"),
            }
        }
        // Esc 关候选，再开 Agent 菜单：菜单按内容收高、不出现多余滚动条，外点关闭
        runtime.notify_frame_input(
            PANEL,
            SURFACE,
            &FrameInput::Key {
                key: atelyx_plugin_protocol::frame::Key::Named {
                    name: atelyx_plugin_protocol::frame::NamedKey::Escape,
                },
                action: atelyx_plugin_protocol::frame::KeyAction::Press,
                modifiers: atelyx_plugin_protocol::frame::Modifiers::default(),
                text: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Some(hit) = runtime
            .surface_frame(PANEL, SURFACE)
            .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "agent-menu").cloned())
        {
            click_hit(&runtime, &hit);
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(frame) = runtime.surface_frame(PANEL, SURFACE) {
                let rows = frame.hits.iter().filter(|hit| hit.id.starts_with("agent:")).count();
                let bar = frame.hits.iter().any(|hit| hit.id == "menu::bar");
                println!(
                    "Agent 菜单：{} 行，滚动条{}",
                    rows,
                    if bar { "出现（不应出现）" } else { "未出现" }
                );
                if let Ok(json) = serde_json::to_vec(&*frame) {
                    let _ = std::fs::write("target/ai_frame_menu.json", json);
                }
            }
            for input in [
                FrameInput::PointerDown {
                    x: 8.0,
                    y: 8.0,
                    button: atelyx_core::plugin_runtime::MouseButton::Left,
                    hit: Some("popup-backdrop".to_string()),
                },
                FrameInput::PointerUp {
                    x: 8.0,
                    y: 8.0,
                    button: atelyx_core::plugin_runtime::MouseButton::Left,
                    hit: Some("popup-backdrop".to_string()),
                },
            ] {
                runtime.notify_frame_input(PANEL, SURFACE, &input);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            let closed = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
                !frame.hits.iter().any(|hit| hit.id.starts_with("agent:"))
                    && !frame.hits.iter().any(|hit| hit.id == "popup-backdrop")
            });
            println!("点外关闭菜单：{}", if closed { "通过" } else { "未关闭" });
        }
    }
    runtime.notify_surface_detached(PANEL, SURFACE);
    runtime.set_enabled(PANEL, false).expect("停用面板");
    runtime.set_enabled(CORE, false).expect("停用核心");
    println!("停用：通过");
    let _ = std::fs::remove_dir_all(&root);
}

fn reply(runtime: &Arc<PluginRuntime>, request_id: u64, value: Value) {
    runtime.resolve_shell_call(request_id, Ok(value));
}

/// 连续改宽到新帧回到宿主的耗时（拖动布局时每一拍都要走这条路径）。
async fn resize_latency(runtime: &Arc<PluginRuntime>, base_width: f32, base_height: f32) {
    wait_resize(runtime, base_width, base_height).expect("初始尺寸");
    let mut samples = Vec::new();
    for step in 0..40 {
        let width = base_width + (step % 10) as f32 * 24.0;
        let started = Instant::now();
        if wait_resize(runtime, width, base_height).is_none() {
            println!("改宽超时：step {step}（{width:.0}）");
            return;
        }
        samples.push(started.elapsed());
    }
    samples.sort();
    let at = |p: f32| samples[((samples.len() - 1) as f32 * p) as usize];
    println!(
        "改宽 {base_width:.0}x{base_height:.0}：p50 {:.2?} / p95 {:.2?} / max {:.2?}",
        at(0.5),
        at(0.95),
        samples.last().unwrap()
    );
}

/// 当前帧里最靠上的消息操作命中纵坐标：滚轮是否让内容上移看它。
fn top_message_y(runtime: &Arc<PluginRuntime>) -> Option<f32> {
    let frame = runtime.surface_frame(PANEL, SURFACE)?;
    frame
        .hits
        .iter()
        .filter(|hit| hit.id.starts_with("copy:") || hit.id.starts_with("rollback:"))
        .map(|hit| hit.y)
        .min_by(f32::total_cmp)
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
        runtime.notify_frame_input(PANEL, SURFACE, &input);
    }
}

fn wait_resize(runtime: &Arc<PluginRuntime>, width: f32, height: f32) -> Option<()> {
    let old = runtime.surface_frame(PANEL, SURFACE)?;
    runtime.notify_frame_input(
        PANEL,
        SURFACE,
        &FrameInput::Resize { width, height },
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(frame) = runtime.surface_frame(PANEL, SURFACE) {
            if !Arc::ptr_eq(&frame, &old) {
                return Some(());
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::hint::spin_loop();
    }
}

fn long_messages() -> Vec<Value> {
    let count = std::env::var("AI_SMOKE_MESSAGES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100);
    let mut messages = Vec::new();
    for index in 0..count {
        let user = index % 2 == 0;
        messages.push(json!({
            "id": format!("m{index}"),
            "role": if user { "user" } else { "assistant" },
            "content": if user {
                format!("第 {index} 条问题：解释一下这段实现里保存链的时序，以及为什么先落盘再合并。")
            } else {
                format!("第 {index} 条回答：\n\n- 第一步先确认磁盘事实\n- 第二步按基线追加\n- 第三步写失败时保留错误并退避重试\n\n```rust\nlet baseline = read(file)?;\nappend(file, baseline, tail)?;\n```\n\n这一段用于让消息流有真实高度。")
            },
            "createdAt": index as i64,
            "steps": if user { json!([]) } else { json!([
                { "kind": "reasoning", "text": "先确认保存链的时序，再决定是追加还是全量重写；同时核对写盘在途、外部修改与程序化替换三者交叉的先后顺序，避免旧快照覆盖新内容，并为每个失败路径留下可定位的原因与用户可见状态。" },
                {
                    "kind": "tool",
                    "run": {
                        "name": "read_file",
                        "status": "done",
                        "argsSummary": "path: src/chat_actions.rs · 1 | --- 2 | tags: 3 | - 开发阶段待补充、待复核",
                        "resultSummary": "已读回 1.2 KB，包含保存链、基线判定与重试退避的完整说明；改宽后摘要必须按可用宽度省略。",
                        "args": { "path": "src/chat_actions.rs" },
                        "result": "fn example() {}\n"
                    }
                }
            ]) },
        }));
    }
    messages
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
        font_sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
        radii: Radii { xs: 4.0, sm: 6.0, md: 8.0 },
        event_colors: vec!["#e06c75".into(), "#61afef".into(), "#98c379".into()],
    }
}

async fn wait_status(runtime: &Arc<PluginRuntime>, id: &str, want: PluginStatus) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if runtime.list().iter().any(|p| p.id == id && p.status == want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("插件 {id} 未进入 {want:?}");
}

async fn wait_frame(runtime: &Arc<PluginRuntime>, timeout: Duration) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(frame) = runtime.surface_frame(PANEL, SURFACE) {
            return Some((*frame).clone());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
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
