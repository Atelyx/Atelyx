//! AI 预置插件的无界面冒烟：装载 ai-core / ai-panel，挂面取帧，再停用 / 卸载。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FrameInput, HostRequest, MeasureTextResult, PluginRuntime, PluginStatus, SurfaceKind,
};
use atelyx_plugin_protocol::event::VaultSwitchPayload;
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
    let root = atelyx_core::scratch::TempDir::new("ai-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
    runtime.scan();
    // 先落下容器身份，后面切到另一个根才会真的触发插件重载
    runtime.on_vault_switch(VaultSwitchPayload::Local {
        root: root.to_string_lossy().into_owned(),
    });

    // 宿主请求应答放在独立线程：改宽基准会忙等，不能占住应答任务所在的运行时
    let counts = Counts::default();
    let clipboard = Arc::new(std::sync::Mutex::new(None::<String>));
    let responder = runtime.clone();
    let responder_counts = counts.clone();
    let responder_clipboard = clipboard.clone();
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
                        responder_counts.models.fetch_add(1, Ordering::Relaxed);
                        reply(
                            &responder,
                            request_id,
                            json!({
                                "providers": [{
                                    "id": "p1",
                                    "name": "测试供应商",
                                    "models": [{ "id": "m1", "nickname": "测试模型" }]
                                }],
                                "current": { "providerId": "p1", "model": "m1" }
                            }),
                        );
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
                        responder_counts.history.fetch_add(1, Ordering::Relaxed);
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
                    // 只回会话 id、不推 Finished：让生成停留在途，供重复 Enter 的忽略判定观察
                    HostRequest::ChatSend { request_id, params, .. } => {
                        responder_counts.sends.fetch_add(1, Ordering::Relaxed);
                        let session = params.session_id.unwrap_or_else(|| "s1".into());
                        reply(
                            &responder,
                            request_id,
                            json!({ "sessionId": session, "messageId": "m1" }),
                        );
                    }
                    HostRequest::ChatStop { request_id, .. } => {
                        reply(&responder, request_id, json!({}));
                    }
                    HostRequest::ChatSessionWrite { request_id, .. } => {
                        reply(&responder, request_id, json!({}));
                    }
                    HostRequest::VaultListDirs { request_id, .. } => {
                        responder_counts.files.fetch_add(1, Ordering::Relaxed);
                        reply(&responder, request_id, json!({ "dirs": ["资料"] }));
                    }
                    HostRequest::VaultFiles { request_id, .. } => {
                        reply(
                            &responder,
                            request_id,
                            json!({
                                "files": [
                                    {
                                        "path": "方案.md",
                                        "name": "方案.md",
                                        "icon": "file",
                                        "openable": true
                                    },
                                    {
                                        "path": "资料/看板.atb",
                                        "name": "看板.atb",
                                        "icon": "table",
                                        "openable": true
                                    }
                                ]
                            }),
                        );
                    }
                    // 打开文件：把「资料/看板.atb」报成打开中，候选里它应排到最前
                    HostRequest::EditorOpenFiles { request_id, .. } => {
                        reply(&responder, request_id, json!({ "paths": ["资料/看板.atb"] }));
                    }
                    HostRequest::VaultExists { request_id, .. } => {
                        reply(&responder, request_id, json!({ "exists": false }));
                    }
                    HostRequest::ClipboardWrite { request_id, text, .. } => {
                        *responder_clipboard.lock().unwrap() = Some(text);
                        reply(&responder, request_id, json!({}));
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
    // 窄面 380：加号 / 发送钮必须留在面宽内（触发钮可收起，不能被裁掉）
    wait_resize(&runtime, 380.0, 700.0).expect("窄面尺寸");
    tokio::time::sleep(Duration::from_millis(100)).await;
    // 空草稿时发送钮不可点（没有命中区），先敲一个字再验位置
    if let Some(draft) = runtime
        .surface_frame(PANEL, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "draft").cloned())
    {
        click_hit(&runtime, &draft);
        type_text(&runtime, "k");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let narrow_ok = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
        ["plus-menu", "send"].iter().all(|id| {
            frame
                .hits
                .iter()
                .find(|hit| hit.id == *id)
                .is_some_and(|hit| hit.x + hit.w <= 380.5)
        })
    });
    assert!(narrow_ok, "窄面 380 下加号与发送钮都应留在面宽内");
    println!("窄面 380 发送钮在面宽内：通过");
    press_backspace(&runtime);
    wait_resize(&runtime, 900.0, 700.0).expect("恢复宽面");
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
    // 在途防重：首次发送后标记在途，第二次 Enter 必须被忽略且停止入口保持
    // 容器切换会清空模型选择，发送前重新选上
    if let Some(menu) = runtime
        .surface_frame(PANEL, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "model-menu").cloned())
    {
        click_hit(&runtime, &menu);
        tokio::time::sleep(Duration::from_millis(120)).await;
        // 模型菜单是二级菜单：先展开「模型」，再点具体模型行
        if let Some(open) = runtime
            .surface_frame(PANEL, SURFACE)
            .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "open-model").cloned())
        {
            click_hit(&runtime, &open);
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        if let Some(row) = runtime
            .surface_frame(PANEL, SURFACE)
            .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "model:0").cloned())
        {
            click_hit(&runtime, &row);
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    if let Some(draft) = runtime
        .surface_frame(PANEL, SURFACE)
        .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "draft").cloned())
    {
        click_hit(&runtime, &draft);
        tokio::time::sleep(Duration::from_millis(50)).await;
        type_text(&runtime, "ping");
        press_enter(&runtime);
        let sent = wait_sends(&counts, 1).await;
        assert!(sent, "首次发送应到达宿主的 ChatSend");
        println!("首次发送：通过");
        type_text(&runtime, "again");
        press_enter(&runtime);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let repeated = counts.sends();
        let stop = runtime
            .surface_frame(PANEL, SURFACE)
            .is_some_and(|frame| frame.hits.iter().any(|hit| hit.id == "stop"));
        assert_eq!(repeated, 1, "在途重复回车不得再发一次 ChatSend");
        assert!(stop, "在途时停止入口应保留");
        println!("在途重复回车：被忽略；停止入口：保留");
        if let Some(hit) = runtime
            .surface_frame(PANEL, SURFACE)
            .and_then(|frame| frame.hits.iter().find(|hit| hit.id == "stop").cloned())
        {
            click_hit(&runtime, &hit);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    // 只读消息选区右键：无选区不弹菜单；拖选后弹「复制」并写入宿主剪贴板
    if let Some(md) = runtime.surface_frame(PANEL, SURFACE).and_then(|frame| {
        frame.hits.iter().find(|hit| hit.id.starts_with("@md:")).cloned()
    }) {
        right_click(&runtime, &md);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let no_menu = runtime.surface_frame(PANEL, SURFACE).is_some_and(|frame| {
            !frame.hits.iter().any(|hit| hit.id == "@sdk:copy-menu")
        });
        assert!(no_menu, "无选区时右键不得弹复制菜单");
        println!("消息选区右键（无选区）：不弹菜单");

        let start = (md.x + 4.0, md.y + 4.0);
        let end = (md.x + md.w - 4.0, md.y + (md.h * 0.7).max(4.0));
        for input in [
            FrameInput::PointerDown {
                x: start.0,
                y: start.1,
                button: atelyx_core::plugin_runtime::MouseButton::Left,
                hit: Some(md.id.clone()),
            },
            FrameInput::PointerMove { x: end.0, y: end.1, hit: None },
            FrameInput::PointerUp {
                x: end.0,
                y: end.1,
                button: atelyx_core::plugin_runtime::MouseButton::Left,
                hit: Some(md.id.clone()),
            },
        ] {
            runtime.notify_frame_input(PANEL, SURFACE, &input);
        }
        right_click(&runtime, &md);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut menu = None;
        while Instant::now() < deadline {
            if let Some(frame) = runtime.surface_frame(PANEL, SURFACE) {
                if frame.hits.iter().any(|hit| hit.id == "@sdk:copy-menu") {
                    menu = Some(frame);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let menu = menu.expect("有选区时右键应弹复制菜单");
        let copy = menu
            .hits
            .iter()
            .find(|hit| hit.id == "@sdk:copy-menu")
            .cloned()
            .expect("复制菜单命中区");
        click_hit(&runtime, &copy);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut copied = None;
        while Instant::now() < deadline {
            if let Some(text) = clipboard.lock().unwrap().clone() {
                copied = Some(text);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let copied = copied.expect("点「复制」应写入宿主剪贴板");
        assert!(!copied.trim().is_empty(), "选区复制内容不应为空");
        println!("消息选区右键复制：{} 字", copied.chars().count());
    }

    // 时序一：已挂面且已订阅时广播，直接重取会话侧；文件树不随会话真源重载重扫
    let base = wait_quiet(&counts, Duration::from_millis(300)).await;
    runtime.on_chat_store_reloaded();
    println!(
        "已挂面收广播即重取（不重扫文件树）：{}",
        if wait_chat_reload(&counts, base).await { "通过" } else { "未重取" }
    );
    // 宿主文件索引重建后候选跟着重取（VAULT_FILES_CHANGED）
    let base = wait_quiet(&counts, Duration::from_millis(300)).await;
    runtime.on_files_changed();
    println!(
        "文件变化重扫候选：{}",
        if wait_files_growth(&counts, base).await { "通过" } else { "未重扫" }
    );

    // 时序二：广播发生在重载之前（面板停用期间）—— 广播被跳过，重新启用并挂面后全量补齐
    runtime.notify_surface_detached(PANEL, SURFACE);
    runtime.set_enabled(PANEL, false).expect("停用面板");
    wait_status(&runtime, PANEL, PluginStatus::Stopped).await;
    let base = wait_quiet(&counts, Duration::from_millis(300)).await;
    runtime.on_chat_store_reloaded();
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!(
        "停用期间广播不重取：{}",
        if counts.snapshot() == base { "通过" } else { "有请求" }
    );
    runtime.set_enabled(PANEL, true).expect("启用面板");
    wait_status(&runtime, PANEL, PluginStatus::Running).await;
    runtime.notify_surface_attached(PANEL, SURFACE, SurfaceKind::Panel);
    runtime.notify_frame_input(PANEL, SURFACE, &FrameInput::Resize { width: 900.0, height: 700.0 });
    println!(
        "重新启用挂面补齐：{}",
        if wait_growth(&counts, base).await { "通过" } else { "未补齐" }
    );

    // 时序三：重载发生在广播之前—— 新实例尚未挂面，广播被门控，挂面后全量补齐
    runtime.on_vault_switch(VaultSwitchPayload::Local {
        root: format!("{}-next", root.to_string_lossy()),
    });
    wait_status(&runtime, PANEL, PluginStatus::Running).await;
    wait_status(&runtime, CORE, PluginStatus::Running).await;
    let base = wait_quiet(&counts, Duration::from_millis(300)).await;
    runtime.on_chat_store_reloaded();
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!(
        "重载后未挂面广播不重取：{}",
        if counts.snapshot() == base { "通过" } else { "有请求" }
    );
    runtime.notify_surface_attached(PANEL, SURFACE, SurfaceKind::Panel);
    runtime.notify_frame_input(PANEL, SURFACE, &FrameInput::Resize { width: 900.0, height: 700.0 });
    println!(
        "重载后挂面补齐：{}",
        if wait_growth(&counts, base).await { "通过" } else { "未补齐" }
    );

    runtime.notify_surface_detached(PANEL, SURFACE);
    runtime.set_enabled(PANEL, false).expect("停用面板");
    runtime.set_enabled(CORE, false).expect("停用核心");
    runtime.shutdown();
    println!("停用：通过");
}

fn reply(runtime: &Arc<PluginRuntime>, request_id: u64, value: Value) {
    runtime.resolve_shell_call(request_id, Ok(value));
}

/// 逐字敲入 ASCII：输入框只认字符键事件。
fn type_text(runtime: &Arc<PluginRuntime>, text: &str) {
    for ch in text.chars() {
        runtime.notify_frame_input(
            PANEL,
            SURFACE,
            &FrameInput::Key {
                key: atelyx_plugin_protocol::frame::Key::Character { text: ch.to_string() },
                action: atelyx_plugin_protocol::frame::KeyAction::Press,
                modifiers: atelyx_plugin_protocol::frame::Modifiers::default(),
                text: Some(ch.to_string()),
            },
        );
    }
}

fn press_enter(runtime: &Arc<PluginRuntime>) {
    runtime.notify_frame_input(
        PANEL,
        SURFACE,
        &FrameInput::Key {
            key: atelyx_plugin_protocol::frame::Key::Named {
                name: atelyx_plugin_protocol::frame::NamedKey::Enter,
            },
            action: atelyx_plugin_protocol::frame::KeyAction::Press,
            modifiers: atelyx_plugin_protocol::frame::Modifiers::default(),
            text: None,
        },
    );
}

fn press_backspace(runtime: &Arc<PluginRuntime>) {
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

/// 观测三类「全量重取」请求：模型清单 / 会话清单 / 仓库目录清单。
#[derive(Clone, Default)]
struct Counts {
    models: Arc<AtomicUsize>,
    history: Arc<AtomicUsize>,
    files: Arc<AtomicUsize>,
    sends: Arc<AtomicUsize>,
}

impl Counts {
    fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.models.load(Ordering::Relaxed),
            self.history.load(Ordering::Relaxed),
            self.files.load(Ordering::Relaxed),
        )
    }

    fn sends(&self) -> usize {
        self.sends.load(Ordering::Relaxed)
    }
}

/// 等发送请求计数达到 `want`（宿主回执后插件才完成这次发送）。
async fn wait_sends(counts: &Counts, want: usize) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if counts.sends() >= want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// 三类请求都超过基线：一次 attach 全量重取会让三项都发出。
async fn wait_growth(counts: &Counts, base: (usize, usize, usize)) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let now = counts.snapshot();
        if now.0 > base.0 && now.1 > base.1 && now.2 > base.2 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// 会话真源重载只重取模型与会话：文件树计数必须保持在基线。
async fn wait_chat_reload(counts: &Counts, base: (usize, usize, usize)) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let now = counts.snapshot();
        if now.0 > base.0 && now.1 > base.1 && now.2 == base.2 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// 文件树重扫：只等 `vault.list` 计数超过基线。
async fn wait_files_growth(counts: &Counts, base: (usize, usize, usize)) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if counts.snapshot().2 > base.2 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// 等计数在安静期内不再变化：避免把启动或上一次交互的在途请求算进基线。
async fn wait_quiet(counts: &Counts, quiet: Duration) -> (usize, usize, usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = counts.snapshot();
    let mut stable_since = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = counts.snapshot();
        if now != last {
            last = now;
            stable_since = Instant::now();
        } else if stable_since.elapsed() >= quiet {
            return last;
        }
        if Instant::now() >= deadline {
            return last;
        }
    }
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

/// 在命中区内按右键（只发按下：SDK 在这一步判定选区与菜单）。
fn right_click(runtime: &Arc<PluginRuntime>, hit: &atelyx_plugin_protocol::frame::HitRegion) {
    let x = hit.x + hit.w / 2.0;
    let y = hit.y + hit.h / 2.0;
    runtime.notify_frame_input(
        PANEL,
        SURFACE,
        &FrameInput::PointerDown {
            x,
            y,
            button: atelyx_core::plugin_runtime::MouseButton::Right,
            hit: Some(hit.id.clone()),
        },
    );
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
