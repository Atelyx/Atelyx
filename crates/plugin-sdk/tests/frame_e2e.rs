//! 整面帧端到端：以真实插件（`frame_stub` 夹具）验证同步绘制落地、命中区随绘制生效，
//! 以及点击 / 滚轮 / 键盘 / 输入法 / 帧拍五条输入通知的往返。

mod support;

use support::{draw, stage};
use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::{
    DrawCommand, FrameInput, Key, KeyAction, Modifiers, MouseButton, PluginRuntime, PluginStatus,
    SurfaceFrame,
};
use atelyx_core::vault::test_support::TempDir;

const PANEL: &str = "frame";

fn install_frame_stub(tmp: &TempDir) -> (std::sync::Arc<PluginRuntime>, String) {
    let runtime = PluginRuntime::new(tmp.join("plugins"));
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    stage(&src, "com.atelyx.frame-stub", "frame_stub", &[], "");
    let id = runtime.install_dir(&src).expect("安装 frame-stub 应成功").id;
    runtime.set_enabled(&id, true).expect("启用应成功");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = runtime
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.status)
            .unwrap_or(PluginStatus::Stopped);
        if status == PluginStatus::Running {
            return (runtime, id);
        }
        assert!(Instant::now() < deadline, "等待 frame-stub 启动超时：{status:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 帧里全部文字按顺序拼起来：夹具把状态写进文字指令，断言只看这一段。
fn frame_text_of(frame: &SurfaceFrame) -> String {
    let mut out = String::new();
    for command in &frame.commands {
        if let DrawCommand::Text { content, .. } = command {
            out.push_str(content);
            out.push('\n');
        }
    }
    out
}

/// 画一面并取回文字：断言始终对着「这一拍真的画出来的东西」。
fn paint(runtime: &PluginRuntime, id: &str, width: f32, height: f32) -> (String, SurfaceFrame) {
    let frame = draw(runtime, id, PANEL, width, height).expect("运行中的插件应画成");
    (frame_text_of(&frame), frame.as_ref().clone())
}

fn send(runtime: &PluginRuntime, id: &str, input: FrameInput) {
    runtime.notify_frame_input(id, PANEL, &input);
}

/// 输入是异步投递到插件线程的：轮询等画面里出现期望片段，别用固定睡眠赌时序。
fn wait_text(runtime: &PluginRuntime, id: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = paint(runtime, id, 320.0, 280.0).0;
        if text.contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "等待画面出现「{needle}」超时，实际：{text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn draw_paints_the_surface_and_inputs_round_trip() {
    let tmp = TempDir::new("plugin-frame-e2e");
    let (runtime, id) = install_frame_stub(&tmp);

    let (text, frame) = paint(&runtime, &id, 320.0, 280.0);
    assert!(text.contains("尺寸：320x280"), "尺寸随绘制给出：{text}");
    assert!(frame.hits.iter().any(|hit| hit.id == "inc"), "命中区随绘制交付");
    assert!(frame.keys, "声明收键盘");
    assert!(frame.ime.is_some(), "声明输入法光标");
    assert_eq!(frame.fps, Some(4), "声明帧拍");

    // 点击：命中 id 由宿主按命中区回发，插件在回调里改状态
    send(&runtime, &id, FrameInput::PointerDown {
        x: 20.0,
        y: 200.0,
        button: MouseButton::Left,
        hit: Some("inc".into()),
    });
    wait_text(&runtime, &id, "计数：1");

    // 滚轮：像素增量直接进状态
    send(&runtime, &id, FrameInput::Wheel {
        x: 40.0,
        y: 270.0,
        delta_x: 0.0,
        delta_y: 24.0,
        hit: None,
    });
    wait_text(&runtime, &id, "滚动：24");

    // 键盘与输入法
    send(&runtime, &id, FrameInput::Key {
        key: Key::Character { text: "a".into() },
        action: KeyAction::Press,
        modifiers: Modifiers::default(),
        text: Some("a".into()),
    });
    wait_text(&runtime, &id, "键入：a");

    send(&runtime, &id, FrameInput::Text {
        text: "中".into(),
        composing: false,
        caret: None,
        candidates: None,
    });
    wait_text(&runtime, &id, "键入：a中");

    // 焦点
    send(&runtime, &id, FrameInput::Focus { focused: true });
    wait_text(&runtime, &id, "焦点：true");

    // 帧拍：宿主按声明的帧率推拍，插件在回调里推进
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        runtime.pump_frame_ticks(Instant::now());
        if paint(&runtime, &id, 320.0, 280.0).0.contains("帧：1") {
            break;
        }
        assert!(Instant::now() < deadline, "等待帧拍推进超时");
        std::thread::sleep(Duration::from_millis(100));
    }

    runtime.set_enabled(&id, false).expect("停用应成功");
    assert!(runtime.surface_frame(&id, PANEL).is_none(), "停用后不得留下可渲染的旧帧");
    assert!(
        draw(&runtime, &id, PANEL, 320.0, 280.0).is_none(),
        "停用后绘制直接跳过"
    );
}

/// 尺寸只从绘制入口来：同一份状态在下一次绘制里按新尺寸重排，不需要额外的尺寸通知。
#[test]
fn size_comes_from_the_draw_target() {
    let tmp = TempDir::new("plugin-frame-size");
    let (runtime, id) = install_frame_stub(&tmp);

    let (text, _) = paint(&runtime, &id, 320.0, 280.0);
    assert!(text.contains("尺寸：320x280"), "{text}");
    let (text, _) = paint(&runtime, &id, 640.0, 400.0);
    assert!(text.contains("尺寸：640x400"), "改宽后同一拍就按新尺寸画出：{text}");
}

/// 命中区按几何取最上面的一条，宿主据此回发 hit id。
#[test]
fn hits_resolve_to_the_topmost_region() {
    let tmp = TempDir::new("plugin-frame-hits");
    let (runtime, id) = install_frame_stub(&tmp);
    let (_, frame) = paint(&runtime, &id, 320.0, 280.0);

    assert_eq!(frame.hit_at(20.0, 200.0), Some("inc"));
    assert_eq!(frame.hit_at(130.0, 200.0), Some("clear"));
    assert_eq!(frame.hit_at(20.0, 240.0), Some("field"));
    assert_eq!(frame.hit_at(300.0, 20.0), Some("surface"));
}
