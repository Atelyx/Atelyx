//! 整面帧端到端：以真实插件进程（`frame-stub` 夹具）验证帧提交落地、命中区随帧生效，
//! 以及点击 / 滚轮 / 键盘 / 输入法 / 尺寸 / 帧拍六条输入通知的往返。

use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::{
    DrawCommand, FrameInput, Key, KeyAction, Modifiers, MouseButton, PluginRuntime, PluginStatus,
};
use atelyx_core::vault::test_support::TempDir;
use atelyx_plugin_sdk::protocol::manifest::MANIFEST_FILE;

fn entry_name() -> &'static str {
    if cfg!(windows) {
        "plugin.exe"
    } else {
        "plugin"
    }
}

fn install_frame_stub(tmp: &TempDir) -> (std::sync::Arc<PluginRuntime>, String) {
    let runtime = PluginRuntime::new(tmp.join("plugins"));
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join(MANIFEST_FILE),
        r#"{"id":"com.atelyx.frame-stub","name":"FrameStub","version":"1.0.0","apiVersion":1,
            "entry":{"windows":"plugin.exe","linux":"plugin"},"permissions":[]}"#,
    )
    .unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_frame_stub"), src.join(entry_name()))
        .expect("拷贝入口可执行文件失败");
    let id = runtime
        .install_dir(&src)
        .expect("安装 frame-stub 应成功")
        .id;
    runtime.set_enabled(&id, true).expect("启用应成功");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = runtime
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.status)
            .unwrap_or(PluginStatus::Stopped);
        if status == PluginStatus::Running && runtime.surface_frame(&id, "frame").is_some() {
            return (runtime, id);
        }
        assert!(Instant::now() < deadline, "等待 frame-stub 提交面帧超时：{status:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 帧里全部文字按顺序拼起来：夹具把状态写进文字指令，断言只看这一段。
fn frame_text(runtime: &PluginRuntime, id: &str) -> String {
    let Some(frame) = runtime.surface_frame(id, "frame") else {
        return String::new();
    };
    let mut out = String::new();
    for command in &frame.commands {
        if let DrawCommand::Text { content, .. } = command {
            out.push_str(content);
            out.push('\n');
        }
    }
    out
}

/// 夹具自报的尺寸变更次数：宿主的尺寸合并看得见的效果就是它涨得比输入慢。
fn resize_count(runtime: &PluginRuntime, id: &str) -> u64 {
    frame_text(runtime, id)
        .lines()
        .find_map(|line| line.strip_prefix("尺寸变更："))
        .and_then(|count| count.trim().parse().ok())
        .expect("夹具应报告尺寸变更次数")
}

/// 轮询等帧文字含指定片段（帧按输入重发，最终内容即最后状态）。
fn wait_text(runtime: &PluginRuntime, id: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = frame_text(runtime, id);
        if text.contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "等待面帧出现「{needle}」超时，实际：{text}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn send(runtime: &PluginRuntime, id: &str, input: FrameInput) {
    runtime.notify_frame_input(id, "frame", &input);
}

#[test]
fn frame_submit_and_raw_input_roundtrip() {
    let tmp = TempDir::new("plugin-frame");
    let (runtime, id) = install_frame_stub(&tmp);

    // 帧内容与声明都原样落地（绘制指令 / 命中区 / 按键 / 输入法 / 帧率）
    let frame = runtime.surface_frame(&id, "frame").expect("应有面帧");
    assert!(frame.keys, "夹具声明收键盘");
    assert!(frame.ime.is_some(), "夹具声明输入法");
    assert_eq!(frame.fps, Some(4));
    let ids: Vec<&str> = frame.hits.iter().map(|hit| hit.id.as_str()).collect();
    assert_eq!(ids, ["surface", "inc", "clear", "field"]);
    assert_eq!(frame.hit_at(20.0, 200.0), Some("inc"), "后声明的命中区优先");

    // 点击：宿主按命中区填 id，插件按 id 自增并重发整帧
    let hit = frame.hit_at(20.0, 200.0).map(str::to_string);
    send(
        &runtime,
        &id,
        FrameInput::PointerDown { x: 20.0, y: 200.0, button: MouseButton::Left, hit },
    );
    wait_text(&runtime, &id, "计数：1");

    // 滚轮：增量按行数换算进插件状态
    send(&runtime, &id, FrameInput::Wheel { x: 40.0, y: 40.0, delta_x: 0.0, delta_y: 2.0, hit: None });
    wait_text(&runtime, &id, "滚动：16");

    // 键盘：具名键与字符键都按协议原样回发
    send(
        &runtime,
        &id,
        FrameInput::Key {
            key: Key::Named { name: atelyx_core::plugin_runtime::NamedKey::Escape },
            action: KeyAction::Press,
            modifiers: Modifiers::default(),
            text: None,
        },
    );
    wait_text(&runtime, &id, "键：named:Escape");
    send(
        &runtime,
        &id,
        FrameInput::Key {
            key: Key::Character { text: "a".into() },
            action: KeyAction::Press,
            modifiers: Modifiers::default(),
            text: Some("a".into()),
        },
    );
    wait_text(&runtime, &id, "键：char:a");

    // 输入法：组合串更新后可见，提交后才落进正文
    send(
        &runtime,
        &id,
        FrameInput::Text { text: "zhong".into(), composing: true, caret: Some(5), candidates: None },
    );
    wait_text(&runtime, &id, "组合：zhong");
    send(
        &runtime,
        &id,
        FrameInput::Text { text: "中".into(), composing: false, caret: None, candidates: None },
    );
    wait_text(&runtime, &id, "键入：中");

    // 尺寸：插件按宿主上报的分配尺寸重排
    send(&runtime, &id, FrameInput::Resize { width: 400.0, height: 300.0 });
    wait_text(&runtime, &id, "尺寸：400x300");

    // 尺寸突发：宿主只保留最新一笔尺寸，插件不必逐条重排
    let before = resize_count(&runtime, &id);
    for step in 1..=20 {
        send(&runtime, &id, FrameInput::Resize { width: 400.0 + step as f32, height: 300.0 });
    }
    wait_text(&runtime, &id, "尺寸：420x300");
    let seen = resize_count(&runtime, &id) - before;
    assert!(seen <= 4, "突发期间只应收到少量最新尺寸，实际 {seen} 次");

    // 焦点：按下取焦、面外按下失焦都由宿主判定后回发
    send(&runtime, &id, FrameInput::Focus { focused: true });
    wait_text(&runtime, &id, "焦点：true");

    // 帧拍：宿主按声明的帧率推拍，插件每拍重发新内容
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        runtime.pump_frame_ticks(Instant::now());
        if frame_text(&runtime, &id).contains("帧：1") {
            break;
        }
        assert!(Instant::now() < deadline, "等待帧拍推进超时");
        std::thread::sleep(Duration::from_millis(100));
    }

    runtime.set_enabled(&id, false).expect("停用应成功");
    assert!(runtime.surface_frame(&id, "frame").is_none(), "停用后不得留下可渲染的旧帧");
}
