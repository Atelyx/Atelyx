//! ABI 参考宿主的冒烟：加载 sample dylib，验证服务回调、帧、输入、panic 与卸载。
//! 用 `bash scripts/plugin-abi-sample.sh` 构建并运行（Windows 与 Linux 同一份代码）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use atelyx_plugin_abi as abi;
use atelyx_plugin_host::{Command, Frame, Host, HostCall, PluginLibrary};

struct Harness {
    seq: Mutex<usize>,
    wait: Condvar,
    latest: Mutex<Option<Frame>>,
    logs: Mutex<Vec<String>>,
    redraws: AtomicUsize,
}

impl Harness {
    fn new() -> Self {
        Self {
            seq: Mutex::new(0),
            wait: Condvar::new(),
            latest: Mutex::new(None),
            logs: Mutex::new(Vec::new()),
            redraws: AtomicUsize::new(0),
        }
    }

    fn seq(&self) -> usize {
        *self.seq.lock().unwrap()
    }

    fn wait_for_frame(&self, after: usize) -> bool {
        let mut seq = self.seq.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while *seq <= after {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let timeout = deadline.saturating_duration_since(now);
            let (guard, _) = self.wait.wait_timeout(seq, timeout).unwrap();
            seq = guard;
        }
        true
    }

    fn snapshot(&self) -> Option<Frame> {
        self.latest.lock().unwrap().clone()
    }

    fn logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }
}

impl Host for Harness {
    fn submit_frame(&self, _surface: &str, frame: Frame) {
        *self.latest.lock().unwrap() = Some(frame);
        *self.seq.lock().unwrap() += 1;
        self.wait.notify_all();
    }

    fn request_redraw(&self, _surface: &str) {
        self.redraws.fetch_add(1, Ordering::Relaxed);
    }

    fn log(&self, level: u32, message: &str) {
        self.logs.lock().unwrap().push(format!("[{level}] {message}"));
    }

    fn call_host(&self, call: HostCall) {
        match call.method.as_str() {
            "app.info" => call.reply(true, "sample-host/1"),
            _ => call.reply(false, "unknown method"),
        }
    }

    fn subscribe(&self, name: &str) {
        self.logs.lock().unwrap().push(format!("[subscribe] {name}"));
    }

    fn reply_service(&self, call_id: u64, ok: bool, payload: &str) {
        self.logs.lock().unwrap().push(format!(
            "[service {call_id}] {} {payload}",
            if ok { "ok" } else { "error" }
        ));
    }
}

fn sample_plugin_path() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let dir = exe.parent().expect("exe dir");
    let name = if cfg!(target_os = "windows") {
        "atelyx_plugin_sample.dll"
    } else if cfg!(target_os = "linux") {
        "libatelyx_plugin_sample.so"
    } else {
        "libatelyx_plugin_sample.dylib"
    };
    dir.join(name)
}

fn input(kind: u32) -> abi::AbiInput {
    abi::AbiInput {
        kind,
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
        button: abi::BUTTON_LEFT,
        key: 0,
        text: abi::AbiStr::EMPTY,
        hit: abi::AbiStr::EMPTY,
        pressed: (kind == abi::INPUT_POINTER_DOWN) as u8,
        modifiers: 0,
        delta_x: 0.0,
        delta_y: 0.0,
        composing: 0,
        has_caret: 0,
        caret: 0,
        candidates: abi::AbiRect::empty_at(0.0, 0.0),
        has_candidates: 0,
    }
}

fn resize(width: f32, height: f32) -> abi::AbiInput {
    let mut value = input(abi::INPUT_RESIZE);
    value.width = width;
    value.height = height;
    value
}

fn pointer(kind: u32, x: f32, y: f32) -> abi::AbiInput {
    let mut value = input(kind);
    value.x = x;
    value.y = y;
    value
}

fn text_input(value: &str) -> abi::AbiInput {
    let mut input = input(abi::INPUT_TEXT);
    input.text = abi::AbiStr::new(value);
    input
}

fn has_text(frame: &Frame, needle: &str) -> bool {
    frame.commands.iter().any(|command| match command {
        Command::Text { content, .. } => content.contains(needle),
        _ => false,
    })
}

fn main() {
    let path = sample_plugin_path();
    let harness = Arc::new(Harness::new());
    let started = Instant::now();
    let library: Arc<PluginLibrary> =
        Arc::new(PluginLibrary::load(&path).expect("加载 sample 动态库"));
    let plugin = library.create(harness.clone()).expect("创建 sample 插件实例");
    println!("加载：{}（{} µs）", path.display(), started.elapsed().as_micros());

    let before = harness.seq();
    plugin.attach("panel", abi::SURFACE_PANEL);
    assert!(harness.wait_for_frame(before), "attach 后未收到帧");
    let before = harness.seq();
    unsafe { plugin.input("panel", &resize(480.0, 320.0)); }
    assert!(harness.wait_for_frame(before), "resize 后未收到帧");
    let frame = harness.snapshot().expect("首帧");
    assert!(has_text(&frame, "sample plugin"), "首帧缺少标题");
    assert!(has_text(&frame, "Service: sample-host/1"), "服务回调结果未进入帧");
    assert!(!frame.hits.is_empty(), "帧缺少命中区");
    println!("首帧：{} 条指令，{} 个命中区", frame.commands.len(), frame.hits.len());

    let before = harness.seq();
    unsafe { plugin.input("panel", &pointer(abi::INPUT_POINTER_DOWN, 30.0, 60.0)); }
    assert!(harness.wait_for_frame(before), "点击后未收到帧");
    let frame = harness.snapshot().expect("点击帧");
    assert!(has_text(&frame, "Clicks: 1"), "点击未计数");
    println!("点击：通过");

    let before = harness.seq();
    unsafe { plugin.input("panel", &text_input("hello")); }
    assert!(harness.wait_for_frame(before), "文本后未收到帧");
    let frame = harness.snapshot().expect("文本帧");
    assert!(has_text(&frame, "Input: hello"), "文本未进入插件状态");
    println!("文本：通过");

    let before = harness.seq();
    plugin.service_call(99, "sample", "echo", "{}");
    assert!(harness.wait_for_frame(before), "服务调用后未收到帧");
    let frame = harness.snapshot().expect("服务调用帧");
    assert!(has_text(&frame, "service sample.echo"), "服务调用未进入插件");
    assert!(
        harness.logs().iter().any(|line| line.contains("[service 99] error")),
        "服务回执未送达宿主"
    );
    println!("服务调用：通过");

    let before = harness.seq();
    plugin.event("test:panic", "");
    assert!(harness.wait_for_frame(before), "panic 后未收到帧");
    let frame = harness.snapshot().expect("panic 帧");
    assert!(has_text(&frame, "Panic caught"), "panic 未在插件边界恢复");
    assert!(harness.logs().iter().any(|line| line.contains("panic caught")), "缺少 panic 日志");
    let before = harness.seq();
    unsafe { plugin.input("panel", &text_input("ok")); }
    assert!(harness.wait_for_frame(before), "panic 后继续输入未收到帧");
    let frame = harness.snapshot().expect("恢复帧");
    assert!(has_text(&frame, "Input: hellook"), "panic 后 worker 未继续");
    println!("panic：捕获并继续");

    let before = harness.seq();
    let started = Instant::now();
    plugin.event("test:bulk", "2000");
    assert!(harness.wait_for_frame(before), "批量帧未收到");
    let bulk_elapsed = started.elapsed();
    let frame = harness.snapshot().expect("批量帧");
    let rects = frame.commands.iter().filter(|command| matches!(command, Command::Rect { .. })).count();
    assert_eq!(rects, 2002, "批量帧矩形数不符");
    println!("批量帧：{} 条指令（响应 {:?}）", frame.commands.len(), bulk_elapsed);

    let mut samples = Vec::with_capacity(2000);
    for index in 0..2000 {
        let before = harness.seq();
        let started = Instant::now();
        unsafe { plugin.input("panel", &pointer(abi::INPUT_POINTER_MOVE, 10.0 + (index % 200) as f32, 120.0)); }
        assert!(harness.wait_for_frame(before), "移动后未收到帧");
        samples.push(started.elapsed());
    }
    samples.sort();
    println!(
        "输入往返（2000 次，含插件线程与帧复制）：p50 {:?} / p95 {:?} / p99 {:?}",
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100],
        samples[samples.len() * 99 / 100]
    );

    let before = harness.seq();
    plugin.tick("panel", 0, 0);
    assert!(harness.wait_for_frame(before), "tick 后未收到帧");
    println!("重绘请求：{}", harness.redraws.load(Ordering::Relaxed));

    drop(plugin);
    println!("卸载：通过");
}
