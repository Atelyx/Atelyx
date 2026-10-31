//! 同进程插件 ABI 的参考实现：worker 线程、整面帧、输入、宿主服务回调与 panic 边界。

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use atelyx_plugin_abi as abi;
use abi::*;

struct SendHost(HostApi);
unsafe impl Send for SendHost {}

struct PluginState {
    tx: Option<Sender<Cmd>>,
    join: Option<JoinHandle<()>>,
}

enum Cmd {
    Attach(String),
    Detach,
    Input(String, OwnedInput),
    Tick,
    Event(String, String),
    Service(u64, String, String, String),
}

struct OwnedInput {
    kind: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    text: String,
    key: u32,
    pressed: bool,
}

struct Worker {
    host: SendHost,
    surface: String,
    width: f32,
    height: f32,
    clicks: u32,
    input: String,
    last: String,
    service: String,
    panics: u32,
    bulk: usize,
}

struct ReplyState {
    result: Mutex<Option<Result<String, String>>>,
    wait: Condvar,
}

impl Worker {
    fn host(&self) -> &HostApi {
        &self.host.0
    }

    fn log(&self, level: u32, text: &str) {
        unsafe { (self.host().log)(self.host().ctx, level, AbiStr::new(text)) };
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Attach(surface) => {
                self.surface = surface;
                self.service = self.query_service();
                self.last = "attached".into();
            }
            Cmd::Detach => {
                self.surface.clear();
                self.last = "detached".into();
            }
            Cmd::Input(surface, input) => {
                if !surface.is_empty() {
                    self.surface = surface;
                }
                self.input_event(input);
            }
            Cmd::Tick => self.last = "tick".into(),
            Cmd::Event(name, payload) => self.event(&name, &payload),
            Cmd::Service(call_id, service, method, _params) => {
                self.last = format!("service {service}.{method}");
                let message = format!("sample 未提供 {service}.{method}");
                unsafe {
                    (self.host().reply_service)(
                        self.host().ctx,
                        call_id,
                        0,
                        AbiStr::new(&message),
                    )
                };
            }
        }
        self.submit();
        // 批量绘制只作用于这一帧，避免后续输入帧被测试用指令撑大。
        self.bulk = 0;
    }

    fn query_service(&self) -> String {
        let state = Arc::new(ReplyState { result: Mutex::new(None), wait: Condvar::new() });
        let reply_ctx = Arc::as_ptr(&state) as *mut c_void;
        // 回执函数持有一份强引用：宿主异步回执时 worker 已超时也不悬空；
        // 宿主永不回执则这份引用只泄漏一次，不影响后续帧。
        unsafe { Arc::increment_strong_count(reply_ctx as *const ReplyState) };
        unsafe {
            (self.host().call_host)(
                self.host().ctx,
                1,
                AbiStr::new("app.info"),
                AbiStr::EMPTY,
                Some(service_reply),
                reply_ctx,
            );
        }
        let mut result = state.result.lock().unwrap();
        if result.is_none() {
            let (guard, _) = state
                .wait
                .wait_timeout(result, Duration::from_secs(2))
                .unwrap();
            result = guard;
        }
        match result.take() {
            Some(Ok(value)) => value,
            Some(Err(error)) => format!("error: {error}"),
            None => "no reply".into(),
        }
    }

    fn input_event(&mut self, input: OwnedInput) {
        match input.kind {
            INPUT_POINTER_DOWN => {
                self.last = format!("pointerDown {:.0},{:.0}", input.x, input.y);
                if (16.0..216.0).contains(&input.x) && (48.0..84.0).contains(&input.y) {
                    self.clicks += 1;
                }
            }
            INPUT_POINTER_UP => self.last = format!("pointerUp {:.0},{:.0}", input.x, input.y),
            INPUT_POINTER_MOVE => self.last = format!("pointerMove {:.0},{:.0}", input.x, input.y),
            INPUT_TEXT => {
                self.input.push_str(&input.text);
                if self.input.len() > 200 {
                    let mut end = 200;
                    while !self.input.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.input.truncate(end);
                }
                self.last = format!("text {}", input.text);
            }
            INPUT_KEY => self.last = format!("key {} pressed {}", input.key, input.pressed),
            INPUT_RESIZE => {
                self.width = input.width;
                self.height = input.height;
                self.last = format!("resize {:.0}x{:.0}", self.width, self.height);
            }
            _ => self.last = format!("input {}", input.kind),
        }
    }

    fn event(&mut self, name: &str, payload: &str) {
        if name == "test:panic" {
            panic!("sample plugin panic");
        }
        if name == "test:bulk" {
            self.bulk = payload.parse::<usize>().unwrap_or(0).min(20_000);
            self.last = format!("bulk {}", self.bulk);
            return;
        }
        self.last = format!("event {name} {payload}");
    }

    fn submit(&self) {
        let mut strings: Vec<String> = Vec::new();
        let mut commands: Vec<AbiCommand> = Vec::new();
        let mut hits: Vec<AbiHit> = Vec::new();
        let width = self.width.max(1.0);
        let height = self.height.max(1.0);

        commands.push(rect(AbiRect::filled(0.0, 0.0, width, height, 0.0, AbiColor::rgba(30, 33, 38, 255))));
        text(&mut strings, &mut commands, "sample plugin", 16.0, 16.0, 20.0, AbiColor::rgba(235, 238, 242, 255));
        commands.push(rect(AbiRect::filled(16.0, 48.0, 200.0, 36.0, 6.0, AbiColor::rgba(58, 110, 165, 255))));
        text(
            &mut strings,
            &mut commands,
            &format!("Clicks: {}", self.clicks),
            28.0,
            58.0,
            14.0,
            AbiColor::rgba(255, 255, 255, 255),
        );
        text(
            &mut strings,
            &mut commands,
            &format!("Input: {}", self.input),
            16.0,
            108.0,
            14.0,
            AbiColor::rgba(180, 188, 198, 255),
        );
        text(
            &mut strings,
            &mut commands,
            &format!("Last: {}", self.last),
            16.0,
            132.0,
            14.0,
            AbiColor::rgba(180, 188, 198, 255),
        );
        text(
            &mut strings,
            &mut commands,
            &format!("Service: {}", self.service),
            16.0,
            156.0,
            14.0,
            AbiColor::rgba(180, 188, 198, 255),
        );
        if self.panics > 0 {
            text(
                &mut strings,
                &mut commands,
                &format!("Panic caught: {}", self.panics),
                16.0,
                180.0,
                14.0,
                AbiColor::rgba(230, 120, 120, 255),
            );
        }
        for index in 0..self.bulk {
            let x = 8.0 + (index % 100) as f32 * 4.0;
            let y = 208.0 + (index / 100) as f32 * 4.0;
            commands.push(rect(AbiRect::filled(x, y, 3.0, 3.0, 0.0, AbiColor::rgba(90, 160, 220, 255))));
        }

        let hit_id = String::from("button");
        hits.push(AbiHit {
            id: AbiStr::new(&hit_id),
            rect: AbiRect::filled(16.0, 48.0, 200.0, 36.0, 6.0, AbiColor::rgba(0, 0, 0, 0)),
            cursor: CURSOR_POINTER,
        });
        let frame = AbiFrame {
            commands: commands.as_ptr(),
            commands_len: commands.len(),
            hits: hits.as_ptr(),
            hits_len: hits.len(),
            has_ime: 0,
            ime: AbiRect::empty_at(0.0, 0.0),
            keys: 0,
            fps: 0,
        };
        unsafe {
            (self.host().submit_frame)(self.host().ctx, AbiStr::new(&self.surface), &frame);
            (self.host().request_redraw)(self.host().ctx, AbiStr::new(&self.surface));
        }
    }
}

fn base(kind: u32, rect: AbiRect) -> AbiCommand {
    AbiCommand {
        kind,
        rect,
        shadow: AbiShadow::NONE,
        line_x2: 0.0,
        line_y2: 0.0,
        circle_cx: 0.0,
        circle_cy: 0.0,
        circle_r: 0.0,
        points: std::ptr::null(),
        points_len: 0,
        closed: 0,
        text: AbiStr::EMPTY,
        text_size: 0.0,
        text_color: AbiColor::TRANSPARENT,
        text_weight: WEIGHT_NORMAL,
        text_family: FAMILY_UI,
        text_align: ALIGN_START,
        text_max_width: 0.0,
        image_source: 0,
        image_path: AbiStr::EMPTY,
        image_local: 0,
        image_fit: FIT_FILL,
    }
}

fn rect(rect: AbiRect) -> AbiCommand {
    base(CMD_RECT, rect)
}

fn text(
    strings: &mut Vec<String>,
    commands: &mut Vec<AbiCommand>,
    value: &str,
    x: f32,
    y: f32,
    size: f32,
    color: AbiColor,
) {
    strings.push(value.to_string());
    let value = strings.last().expect("just pushed");
    let mut command = base(CMD_TEXT, AbiRect::empty_at(x, y));
    command.text = AbiStr::new(value);
    command.text_size = size;
    command.text_color = color;
    commands.push(command);
}

unsafe extern "C" fn service_reply(
    ctx: *mut c_void,
    _request_id: u64,
    ok: u8,
    payload: AbiStr,
) {
    if ctx.is_null() {
        return;
    }
    let value = copy_str(payload);
    let state = unsafe { Arc::from_raw(ctx as *const ReplyState) };
    *state.result.lock().unwrap() = Some(if ok != 0 { Ok(value) } else { Err(value) });
    state.wait.notify_all();
}

fn copy_str(value: AbiStr) -> String {
    if value.data.is_null() || value.len == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.data, value.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn worker_loop(mut worker: Worker, rx: Receiver<Cmd>) {
    while let Ok(cmd) = rx.recv() {
        let result = catch_unwind(AssertUnwindSafe(|| worker.handle(cmd)));
        if result.is_err() {
            worker.panics += 1;
            worker.last = "panic caught".into();
            worker.log(LOG_ERROR, "panic caught at plugin boundary");
            worker.submit();
        }
    }
    worker.log(LOG_INFO, "worker stopped");
}

fn state<'a>(handle: *mut c_void) -> Option<&'a mut PluginState> {
    if handle.is_null() {
        None
    } else {
        Some(unsafe { &mut *(handle as *mut PluginState) })
    }
}

unsafe extern "C" fn create(host: *const HostApi) -> *mut c_void {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if host.is_null() {
            return std::ptr::null_mut();
        }
        let host = unsafe { *host };
        if host.abi_version != ABI_VERSION {
            return std::ptr::null_mut();
        }
        let (tx, rx) = mpsc::channel();
        let worker = Worker {
            host: SendHost(host),
            surface: String::new(),
            width: 0.0,
            height: 0.0,
            clicks: 0,
            input: String::new(),
            last: "created".into(),
            service: String::new(),
            panics: 0,
            bulk: 0,
        };
        let join = match thread::Builder::new()
            .name("sample-plugin".into())
            .spawn(move || worker_loop(worker, rx))
        {
            Ok(join) => join,
            Err(_) => return std::ptr::null_mut(),
        };
        Box::into_raw(Box::new(PluginState { tx: Some(tx), join: Some(join) }))
    }));
    result.unwrap_or(std::ptr::null_mut()) as *mut c_void
}

unsafe extern "C" fn destroy(handle: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(state) = state(handle) else {
            return;
        };
        let tx = state.tx.take();
        drop(tx);
        if let Some(join) = state.join.take() {
            let _ = join.join();
        }
        unsafe { drop(Box::from_raw(handle as *mut PluginState)) };
    }));
}

unsafe extern "C" fn on_attach(handle: *mut c_void, surface: AbiStr, _kind: u32) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            if let Some(tx) = &state.tx {
                let _ = tx.send(Cmd::Attach(copy_str(surface)));
            }
        }
    }));
}

unsafe extern "C" fn on_detach(handle: *mut c_void, _surface: AbiStr) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            if let Some(tx) = &state.tx {
                let _ = tx.send(Cmd::Detach);
            }
        }
    }));
}

unsafe extern "C" fn on_input(handle: *mut c_void, surface: AbiStr, input: *const AbiInput) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(state) = state(handle) else {
            return;
        };
        let Some(input) = (if input.is_null() { None } else { Some(unsafe { *input }) }) else {
            return;
        };
        if let Some(tx) = &state.tx {
            let _ = tx.send(Cmd::Input(
                copy_str(surface),
                OwnedInput {
                    kind: input.kind,
                    x: input.x,
                    y: input.y,
                    width: input.width,
                    height: input.height,
                    text: copy_str(input.text),
                    key: input.key,
                    pressed: input.pressed != 0,
                },
            ));
        }
    }));
}

unsafe extern "C" fn on_tick(handle: *mut c_void, _surface: AbiStr, _frame: u64, _delta_ms: u32) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            if let Some(tx) = &state.tx {
                let _ = tx.send(Cmd::Tick);
            }
        }
    }));
}

unsafe extern "C" fn on_event(handle: *mut c_void, name: AbiStr, payload: AbiStr) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            if let Some(tx) = &state.tx {
                let _ = tx.send(Cmd::Event(copy_str(name), copy_str(payload)));
            }
        }
    }));
}

unsafe extern "C" fn on_service_call(
    handle: *mut c_void,
    call_id: u64,
    service: AbiStr,
    method: AbiStr,
    params: AbiStr,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            if let Some(tx) = &state.tx {
                let _ = tx.send(Cmd::Service(
                    call_id,
                    copy_str(service),
                    copy_str(method),
                    copy_str(params),
                ));
            }
        }
    }));
}

static PLUGIN_API: PluginApi = PluginApi {
    abi_version: ABI_VERSION,
    create,
    destroy,
    on_attach,
    on_detach,
    on_input,
    on_tick,
    on_event,
    on_service_call,
};

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const PluginApi {
    &PLUGIN_API
}
