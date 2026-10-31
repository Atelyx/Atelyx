//! 同进程后端：把插件注册到 ABI 函数表，能力请求走宿主回调、整面帧直接交宿主复制。
//! 插件作者仍只实现 [`PluginHandler`]；本模块负责握手、事件分派、worker 串行与 panic 边界。

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use atelyx_plugin_abi as abi;
use serde_json::Value;

use crate::context::{Context, FrameSubmit, LogSink, PendingHandle, ServiceReply};
use crate::handler::{self, PluginHandler};
use crate::protocol::envelope::{Envelope, RpcError};
use crate::protocol::frame::{
    FrameInput, FrameRect, HitCursor, Key, KeyAction, Modifiers, MouseButton, NamedKey,
    SurfaceFrame,
};
use crate::protocol::handshake::{self, InitializeParams, PluginInfo};
use crate::protocol::method::{SubmitResult, SurfaceKind};
use crate::protocol::paint::{
    ColorToken, DrawCommand, ImageFit, ImageSource, PaintColor, TextAlign, TextFamily, TextWeight,
};
use crate::protocol::service::HostCallParams;

const INIT_TIMEOUT: Duration = Duration::from_secs(10);

type Factory = fn() -> Box<dyn PluginHandler>;

static INFO: OnceLock<fn() -> PluginInfo> = OnceLock::new();
static FACTORY: OnceLock<Factory> = OnceLock::new();
static API: OnceLock<abi::PluginApi> = OnceLock::new();

/// 插件导出的唯一入口：登记身份与处理器工厂，返回稳定的 ABI 函数表。
pub fn plugin_api(info: fn() -> PluginInfo, factory: Factory) -> *const abi::PluginApi {
    let _ = INFO.set(info);
    let _ = FACTORY.set(factory);
    API.get_or_init(|| abi::PluginApi {
        abi_version: abi::ABI_VERSION,
        create,
        destroy,
        on_attach,
        on_detach,
        on_input,
        on_tick,
        on_event,
        on_service_call,
    })
}

struct PluginRuntimeState {
    tx: std_mpsc::Sender<Cmd>,
    worker: JoinHandle<()>,
    pump: JoinHandle<()>,
}

enum Cmd {
    Attach(String, u32),
    Detach(String),
    Input(String, FrameInput),
    Tick(String, u64, u32),
    Event(String, String),
    Service { call_id: u64, service: String, method: String, params: String },
    Stop,
}

#[derive(Clone, Copy)]
struct HostBridge {
    api: abi::HostApi,
}

// ABI 约定 HostApi 可从任意插件线程调用；HostBridge 只保存函数表值。
unsafe impl Send for HostBridge {}
unsafe impl Sync for HostBridge {}

impl HostBridge {
    fn call_host(
        &self,
        request_id: u64,
        method: &str,
        params: &str,
        reply: Option<abi::HostReplyFn>,
        reply_ctx: *mut c_void,
    ) {
        unsafe {
            (self.api.call_host)(
                self.api.ctx,
                request_id,
                abi::AbiStr::new(method),
                abi::AbiStr::new(params),
                reply,
                reply_ctx,
            )
        }
    }

    fn log(&self, level: u32, message: &str) {
        unsafe { (self.api.log)(self.api.ctx, level, abi::AbiStr::new(message)) }
    }

    fn submit(&self, surface: &str, frame: SurfaceFrame) -> Result<SubmitResult, RpcError> {
        frame
            .validate()
            .map_err(|error| RpcError::failed(format!("帧不合法：{error}")))?;
        let buffer = FrameBuffer::new(&frame);
        let raw = buffer.raw();
        unsafe { (self.api.submit_frame)(self.api.ctx, abi::AbiStr::new(surface), &raw) };
        Ok(SubmitResult { applied: true })
    }

    fn reply_service(&self, call_id: &str, ok: bool, payload: &str) -> Result<(), RpcError> {
        let call_id = call_id
            .parse::<u64>()
            .map_err(|_| RpcError::failed(format!("服务回执的 callId 非法：{call_id}")))?;
        unsafe {
            (self.api.reply_service)(
                self.api.ctx,
                call_id,
                ok as u8,
                abi::AbiStr::new(payload),
            )
        };
        Ok(())
    }
}

struct ReplyCtx {
    pending: PendingHandle,
    id: u64,
}

unsafe extern "C" fn reply(ctx: *mut c_void, _request_id: u64, ok: u8, payload: abi::AbiStr) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() {
            return;
        }
        let reply = unsafe { Box::from_raw(ctx as *mut ReplyCtx) };
        let text = copy_str(payload);
        let outcome = if ok != 0 {
            if text.trim().is_empty() {
                Ok(Value::Null)
            } else {
                serde_json::from_str::<Value>(&text)
                    .map_err(|error| RpcError::failed(format!("宿主应答不可解析：{error}")))
            }
        } else {
            Err(RpcError::failed(text))
        };
        if let Some(sender) = reply.pending.remove(reply.id) {
            let _ = sender.send(outcome);
        }
    }));
}

fn pump(host: HostBridge, pending: PendingHandle, mut rx: tokio::sync::mpsc::Receiver<Envelope>) {
    while let Some(envelope) = rx.blocking_recv() {
        let Envelope::Request(request) = envelope else {
            continue;
        };
        let id = request.id;
        let params = if request.params.is_null() {
            String::new()
        } else {
            serde_json::to_string(&request.params).unwrap_or_else(|_| "null".into())
        };
        let reply_ctx = Box::into_raw(Box::new(ReplyCtx { pending: pending.clone(), id })) as *mut c_void;
        host.call_host(id, &request.method, &params, Some(reply), reply_ctx);
    }
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(RpcError::failed("插件连接已结束")));
    }
}

fn worker_loop(
    host: HostBridge,
    ctx: Context,
    info: PluginInfo,
    factory: Factory,
    rx: std_mpsc::Receiver<Cmd>,
    ready: std_mpsc::Sender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(format!("创建插件运行时失败：{error}")));
            return;
        }
    };
    let handler = match catch_unwind(AssertUnwindSafe(factory)) {
        Ok(handler) => handler,
        Err(_) => {
            host.log(abi::LOG_ERROR, "插件构造 panic");
            let _ = ready.send(Err("插件构造 panic".into()));
            return;
        }
    };
    let started = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async {
            let params = InitializeParams { api_version: crate::protocol::API_VERSION, plugin: info };
            let params = serde_json::to_value(&params)
                .map_err(|error| RpcError::failed(format!("握手参数序列化失败：{error}")))?;
            ctx.call_with_timeout(handshake::INITIALIZE, params, INIT_TIMEOUT).await?;
            handler.on_ready(&ctx).await;
            Ok::<(), RpcError>(())
        })
    }));
    match started {
        Ok(Ok(())) => {
            let _ = ready.send(Ok(()));
        }
        Ok(Err(error)) => {
            host.log(abi::LOG_ERROR, &format!("插件初始化失败：{error}"));
            // 初始化中途失败也要走收尾：on_ready 起的线程可能持有 Context，泵线程等它们退出
            let _ = catch_unwind(AssertUnwindSafe(|| runtime.block_on(handler.on_shutdown(&ctx))));
            let _ = ready.send(Err(error.to_string()));
            return;
        }
        Err(_) => {
            host.log(abi::LOG_ERROR, "插件初始化 panic");
            let _ = catch_unwind(AssertUnwindSafe(|| runtime.block_on(handler.on_shutdown(&ctx))));
            let _ = ready.send(Err("插件初始化 panic".into()));
            return;
        }
    }
    while let Ok(cmd) = rx.recv() {
        if matches!(&cmd, Cmd::Stop) {
            let _ = catch_unwind(AssertUnwindSafe(|| runtime.block_on(handler.on_shutdown(&ctx))));
            break;
        }
        let handled =
            catch_unwind(AssertUnwindSafe(|| runtime.block_on(handle_cmd(handler.as_ref(), &ctx, cmd))));
        if handled.is_err() {
            host.log(abi::LOG_ERROR, "插件入口 panic，已跳过本次输入");
        }
    }
}

async fn handle_cmd(handler: &dyn PluginHandler, ctx: &Context, cmd: Cmd) {
    match cmd {
        Cmd::Attach(surface_id, kind) => {
            handler
                .on_surface_attached(
                    ctx,
                    crate::protocol::method::SurfaceAttachedParams {
                        surface_id,
                        kind: surface_kind(kind),
                    },
                )
                .await;
        }
        Cmd::Detach(surface_id) => {
            handler
                .on_surface_detached(
                    ctx,
                    crate::protocol::method::SurfaceDetachedParams { surface_id },
                )
                .await;
        }
        Cmd::Input(surface_id, input) => {
            handler
                .on_frame_input(
                    ctx,
                    crate::protocol::frame::FrameInputParams { surface_id, input },
                )
                .await;
        }
        Cmd::Tick(surface_id, frame, delta_ms) => {
            handler
                .on_frame_tick(
                    ctx,
                    crate::protocol::frame::FrameTickParams { surface_id, frame, delta_ms },
                )
                .await;
        }
        Cmd::Event(name, payload) => {
            let params = if payload.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&payload).unwrap_or_else(|error| {
                    Value::String(format!("载荷不可解析：{error}"))
                })
            };
            handler::dispatch(handler, ctx, &name, params).await;
        }
        Cmd::Service { call_id, service, method, params } => {
            let params = if params.trim().is_empty() {
                Value::Null
            } else {
                match serde_json::from_str(&params) {
                    Ok(params) => params,
                    Err(error) => {
                        let _ = ctx
                            .service_error(&call_id.to_string(), &format!("服务调用参数不是合法 JSON：{error}"))
                            .await;
                        return;
                    }
                }
            };
            handler
                .on_service_call(
                    ctx,
                    HostCallParams { call_id: call_id.to_string(), service, method, params },
                )
                .await;
        }
        Cmd::Stop => {}
    }
}

fn state<'a>(handle: *mut c_void) -> Option<&'a PluginRuntimeState> {
    if handle.is_null() {
        None
    } else {
        Some(unsafe { &*(handle as *const PluginRuntimeState) })
    }
}

unsafe extern "C" fn create(host: *const abi::HostApi) -> *mut c_void {
    catch_unwind(AssertUnwindSafe(|| create_inner(host))).unwrap_or(std::ptr::null_mut())
}

unsafe fn create_inner(host: *const abi::HostApi) -> *mut c_void {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let api = unsafe { *host };
    if api.abi_version != abi::ABI_VERSION {
        return std::ptr::null_mut();
    }
    let Some(info) = INFO.get().map(|info| info()) else {
        return std::ptr::null_mut();
    };
    let Some(factory) = FACTORY.get().copied() else {
        return std::ptr::null_mut();
    };
    let host = HostBridge { api };
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(64);
    let submitter: FrameSubmit =
        Arc::new(move |surface: &str, frame: SurfaceFrame| host.submit(surface, frame));
    let service_reply: ServiceReply =
        Arc::new(move |call_id: &str, ok: bool, payload: &str| host.reply_service(call_id, ok, payload));
    let log: LogSink = Arc::new(move |message: &str| host.log(abi::LOG_ERROR, message));
    let ctx = Context::new_in_process(out_tx, submitter, service_reply, log);
    let pending = ctx.pending_handle();
    let (cmd_tx, cmd_rx) = std_mpsc::channel();
    let (ready_tx, ready_rx) = std_mpsc::channel();
    let pump = match thread::Builder::new()
        .name("atelyx-plugin-rpc".into())
        .spawn(move || pump(host, pending, out_rx))
    {
        Ok(handle) => handle,
        Err(error) => {
            host.log(abi::LOG_ERROR, &format!("启动插件请求线程失败：{error}"));
            return std::ptr::null_mut();
        }
    };
    let worker = match thread::Builder::new()
        .name("atelyx-plugin-worker".into())
        .spawn(move || worker_loop(host, ctx, info, factory, cmd_rx, ready_tx))
    {
        Ok(handle) => handle,
        Err(error) => {
            drop(cmd_tx);
            let _ = pump.join();
            host.log(abi::LOG_ERROR, &format!("启动插件工作线程失败：{error}"));
            return std::ptr::null_mut();
        }
    };
    let started = ready_rx.recv_timeout(INIT_TIMEOUT + Duration::from_secs(1));
    match started {
        Ok(Ok(())) => Box::into_raw(Box::new(PluginRuntimeState { tx: cmd_tx, worker, pump })) as *mut c_void,
        Ok(Err(error)) => {
            let _ = cmd_tx.send(Cmd::Stop);
            let _ = worker.join();
            let _ = pump.join();
            host.log(abi::LOG_ERROR, &format!("插件启动失败：{error}"));
            std::ptr::null_mut()
        }
        Err(error) => {
            let _ = cmd_tx.send(Cmd::Stop);
            let _ = worker.join();
            let _ = pump.join();
            host.log(abi::LOG_ERROR, &format!("插件初始化超时：{error}"));
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn destroy(handle: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return;
        }
        let state = unsafe { Box::from_raw(handle as *mut PluginRuntimeState) };
        let _ = state.tx.send(Cmd::Stop);
        let _ = state.worker.join();
        let _ = state.pump.join();
    }));
}

unsafe extern "C" fn on_attach(handle: *mut c_void, surface: abi::AbiStr, kind: u32) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            let _ = state.tx.send(Cmd::Attach(copy_str(surface), kind));
        }
    }));
}

unsafe extern "C" fn on_detach(handle: *mut c_void, surface: abi::AbiStr) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            let _ = state.tx.send(Cmd::Detach(copy_str(surface)));
        }
    }));
}

unsafe extern "C" fn on_input(handle: *mut c_void, surface: abi::AbiStr, input: *const abi::AbiInput) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if input.is_null() {
            return;
        }
        let Some(input) = input_from_abi(unsafe { &*input }) else {
            return;
        };
        if let Some(state) = state(handle) {
            let _ = state.tx.send(Cmd::Input(copy_str(surface), input));
        }
    }));
}

unsafe extern "C" fn on_tick(handle: *mut c_void, surface: abi::AbiStr, frame: u64, delta_ms: u32) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            let _ = state.tx.send(Cmd::Tick(copy_str(surface), frame, delta_ms));
        }
    }));
}

unsafe extern "C" fn on_event(handle: *mut c_void, name: abi::AbiStr, payload: abi::AbiStr) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            let _ = state.tx.send(Cmd::Event(copy_str(name), copy_str(payload)));
        }
    }));
}

unsafe extern "C" fn on_service_call(
    handle: *mut c_void,
    call_id: u64,
    service: abi::AbiStr,
    method: abi::AbiStr,
    params: abi::AbiStr,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(state) = state(handle) {
            let _ = state.tx.send(Cmd::Service {
                call_id,
                service: copy_str(service),
                method: copy_str(method),
                params: copy_str(params),
            });
        }
    }));
}

fn copy_str(value: abi::AbiStr) -> String {
    if value.is_empty() {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.data, value.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn surface_kind(code: u32) -> SurfaceKind {
    match code {
        abi::SURFACE_WINDOW => SurfaceKind::Window,
        abi::SURFACE_PAGE => SurfaceKind::Page,
        abi::SURFACE_TAKEOVER => SurfaceKind::Takeover,
        abi::SURFACE_SETTINGS => SurfaceKind::Settings,
        abi::SURFACE_TABLE_VIEW => SurfaceKind::TableView,
        _ => SurfaceKind::Panel,
    }
}

fn input_from_abi(input: &abi::AbiInput) -> Option<FrameInput> {
    let hit = Some(copy_str(input.hit)).filter(|hit| !hit.is_empty());
    Some(match input.kind {
        abi::INPUT_POINTER_DOWN => FrameInput::PointerDown {
            x: input.x,
            y: input.y,
            button: button_from_code(input.button),
            hit,
        },
        abi::INPUT_POINTER_UP => FrameInput::PointerUp {
            x: input.x,
            y: input.y,
            button: button_from_code(input.button),
            hit,
        },
        abi::INPUT_POINTER_MOVE => FrameInput::PointerMove { x: input.x, y: input.y, hit },
        abi::INPUT_POINTER_LEAVE => FrameInput::PointerLeave,
        abi::INPUT_DOUBLE_CLICK => FrameInput::DoubleClick {
            x: input.x,
            y: input.y,
            button: button_from_code(input.button),
            hit,
        },
        abi::INPUT_WHEEL => FrameInput::Wheel {
            x: input.x,
            y: input.y,
            delta_x: input.delta_x,
            delta_y: input.delta_y,
            hit,
        },
        abi::INPUT_KEY => {
            let text = copy_str(input.text);
            let key = if input.key == abi::KEY_CHARACTER {
                Key::Character { text: text.clone() }
            } else {
                Key::Named { name: named_key(input.key)? }
            };
            FrameInput::Key {
                key,
                action: if input.pressed != 0 { KeyAction::Press } else { KeyAction::Release },
                modifiers: modifiers_from_bits(input.modifiers),
                text: (!text.is_empty()).then_some(text),
            }
        }
        abi::INPUT_TEXT => FrameInput::Text {
            text: copy_str(input.text),
            composing: input.composing != 0,
            caret: (input.has_caret != 0).then_some(input.caret),
            candidates: (input.has_candidates != 0).then_some(FrameRect {
                x: input.candidates.x,
                y: input.candidates.y,
                w: input.candidates.w,
                h: input.candidates.h,
            }),
        },
        abi::INPUT_FOCUS => FrameInput::Focus { focused: input.pressed != 0 },
        abi::INPUT_RESIZE => FrameInput::Resize { width: input.width, height: input.height },
        _ => return None,
    })
}

fn button_from_code(code: u32) -> MouseButton {
    match code {
        abi::BUTTON_RIGHT => MouseButton::Right,
        abi::BUTTON_MIDDLE => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn modifiers_from_bits(bits: u32) -> Modifiers {
    Modifiers {
        shift: bits & abi::MOD_SHIFT != 0,
        control: bits & abi::MOD_CONTROL != 0,
        alt: bits & abi::MOD_ALT != 0,
        meta: bits & abi::MOD_META != 0,
    }
}

fn named_key(code: u32) -> Option<NamedKey> {
    Some(match code {
        abi::KEY_ESCAPE => NamedKey::Escape,
        abi::KEY_ENTER => NamedKey::Enter,
        abi::KEY_TAB => NamedKey::Tab,
        abi::KEY_SPACE => NamedKey::Space,
        abi::KEY_BACKSPACE => NamedKey::Backspace,
        abi::KEY_DELETE => NamedKey::Delete,
        abi::KEY_INSERT => NamedKey::Insert,
        abi::KEY_HOME => NamedKey::Home,
        abi::KEY_END => NamedKey::End,
        abi::KEY_PAGE_UP => NamedKey::PageUp,
        abi::KEY_PAGE_DOWN => NamedKey::PageDown,
        abi::KEY_ARROW_UP => NamedKey::ArrowUp,
        abi::KEY_ARROW_DOWN => NamedKey::ArrowDown,
        abi::KEY_ARROW_LEFT => NamedKey::ArrowLeft,
        abi::KEY_ARROW_RIGHT => NamedKey::ArrowRight,
        abi::KEY_F1 => NamedKey::F1,
        abi::KEY_F2 => NamedKey::F2,
        abi::KEY_F3 => NamedKey::F3,
        abi::KEY_F4 => NamedKey::F4,
        abi::KEY_F5 => NamedKey::F5,
        abi::KEY_F6 => NamedKey::F6,
        abi::KEY_F7 => NamedKey::F7,
        abi::KEY_F8 => NamedKey::F8,
        abi::KEY_F9 => NamedKey::F9,
        abi::KEY_F10 => NamedKey::F10,
        abi::KEY_F11 => NamedKey::F11,
        abi::KEY_F12 => NamedKey::F12,
        _ => return None,
    })
}

/// 帧与其字符串 / 点集在提交调用内保持有效；宿主复制后即可释放。
struct FrameBuffer {
    strings: Vec<String>,
    points: Vec<Vec<abi::AbiPoint>>,
    commands: Vec<abi::AbiCommand>,
    hits: Vec<abi::AbiHit>,
    ime: Option<abi::AbiRect>,
    keys: bool,
    fps: u32,
}

impl FrameBuffer {
    fn new(frame: &SurfaceFrame) -> Self {
        let mut buffer = Self {
            strings: Vec::new(),
            points: Vec::new(),
            commands: Vec::with_capacity(frame.commands.len()),
            hits: Vec::with_capacity(frame.hits.len()),
            ime: frame.ime.map(|rect| {
                abi::AbiRect::filled(rect.x, rect.y, rect.w, rect.h, 0.0, abi::AbiColor::TRANSPARENT)
            }),
            keys: frame.keys,
            fps: frame.fps.map(u32::from).unwrap_or(0),
        };
        for command in &frame.commands {
            buffer.command(command);
        }
        for hit in &frame.hits {
            let id = buffer.store(&hit.id);
            buffer.hits.push(abi::AbiHit {
                id,
                rect: abi::AbiRect::filled(
                    hit.x,
                    hit.y,
                    hit.w,
                    hit.h,
                    0.0,
                    abi::AbiColor::TRANSPARENT,
                ),
                cursor: cursor_code(hit.cursor.unwrap_or_default()),
            });
        }
        buffer
    }

    fn raw(&self) -> abi::AbiFrame {
        abi::AbiFrame {
            commands: self.commands.as_ptr(),
            commands_len: self.commands.len(),
            hits: self.hits.as_ptr(),
            hits_len: self.hits.len(),
            has_ime: self.ime.is_some() as u8,
            ime: self.ime.unwrap_or_else(|| abi::AbiRect::empty_at(0.0, 0.0)),
            keys: self.keys as u8,
            fps: self.fps,
        }
    }

    fn store(&mut self, value: &str) -> abi::AbiStr {
        self.strings.push(value.to_string());
        abi::AbiStr::new(self.strings.last().expect("just pushed"))
    }

    fn command(&mut self, command: &DrawCommand) {
        match command {
            DrawCommand::Line { x1, y1, x2, y2, stroke, width } => {
                let mut value = base(abi::CMD_LINE, rect(*x1, *y1, 0.0, 0.0, 0.0));
                value.rect.stroke = color(*stroke);
                value.rect.has_stroke = 1;
                value.rect.stroke_width = *width;
                value.line_x2 = *x2;
                value.line_y2 = *y2;
                self.commands.push(value);
            }
            DrawCommand::Rect {
                x,
                y,
                w,
                h,
                fill,
                stroke,
                stroke_width,
                corner_radius,
                shadow,
            } => {
                let mut value = base(
                    abi::CMD_RECT,
                    rect(*x, *y, *w, *h, corner_radius.unwrap_or(0.0)),
                );
                set_fill_stroke(&mut value.rect, *fill, *stroke, *stroke_width);
                if let Some(shadow) = shadow {
                    value.shadow = abi::AbiShadow {
                        offset_x: shadow.offset_x,
                        offset_y: shadow.offset_y,
                        blur: shadow.blur,
                        color: color(shadow.color),
                        enabled: 1,
                    };
                }
                self.commands.push(value);
            }
            DrawCommand::Path { points, closed, fill, stroke, stroke_width } => {
                self.points.push(
                    points
                        .iter()
                        .map(|point| abi::AbiPoint { x: point.x, y: point.y })
                        .collect(),
                );
                let mut value = base(abi::CMD_PATH, abi::AbiRect::empty_at(0.0, 0.0));
                set_fill_stroke(&mut value.rect, *fill, *stroke, *stroke_width);
                let points = self.points.last().expect("just pushed");
                value.points = points.as_ptr();
                value.points_len = points.len();
                value.closed = *closed as u8;
                self.commands.push(value);
            }
            DrawCommand::Circle { cx, cy, r, fill, stroke, stroke_width } => {
                let mut value = base(abi::CMD_CIRCLE, abi::AbiRect::empty_at(0.0, 0.0));
                set_fill_stroke(&mut value.rect, *fill, *stroke, *stroke_width);
                value.circle_cx = *cx;
                value.circle_cy = *cy;
                value.circle_r = *r;
                self.commands.push(value);
            }
            DrawCommand::Text {
                x,
                y,
                content,
                size,
                color: text_color,
                weight,
                family,
                max_width,
                align,
            } => {
                let text = self.store(content);
                let mut value = base(abi::CMD_TEXT, abi::AbiRect::empty_at(*x, *y));
                value.text = text;
                value.text_size = *size;
                value.text_color = color(*text_color);
                value.text_weight = weight_code(*weight);
                value.text_family = family_code(*family);
                value.text_align = align_code(*align);
                value.text_max_width = max_width.unwrap_or(0.0);
                self.commands.push(value);
            }
            DrawCommand::Image { source, x, y, w, h, fit } => {
                let (kind, path, local) = image_source(source);
                let path = self.store(&path);
                let mut value = base(abi::CMD_IMAGE, rect(*x, *y, *w, *h, 0.0));
                value.image_source = kind;
                value.image_path = path;
                value.image_local = local as u8;
                value.image_fit = fit_code(*fit);
                self.commands.push(value);
            }
        }
    }
}

fn base(kind: u32, rect: abi::AbiRect) -> abi::AbiCommand {
    abi::AbiCommand {
        kind,
        rect,
        shadow: abi::AbiShadow::NONE,
        line_x2: 0.0,
        line_y2: 0.0,
        circle_cx: 0.0,
        circle_cy: 0.0,
        circle_r: 0.0,
        points: std::ptr::null(),
        points_len: 0,
        closed: 0,
        text: abi::AbiStr::EMPTY,
        text_size: 0.0,
        text_color: abi::AbiColor::TRANSPARENT,
        text_weight: abi::WEIGHT_NORMAL,
        text_family: abi::FAMILY_UI,
        text_align: abi::ALIGN_START,
        text_max_width: 0.0,
        image_source: 0,
        image_path: abi::AbiStr::EMPTY,
        image_local: 0,
        image_fit: abi::FIT_FILL,
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32, radius: f32) -> abi::AbiRect {
    abi::AbiRect::filled(x, y, w, h, radius, abi::AbiColor::TRANSPARENT)
}

fn set_fill_stroke(
    rect: &mut abi::AbiRect,
    fill: Option<PaintColor>,
    stroke: Option<PaintColor>,
    stroke_width: Option<f32>,
) {
    if let Some(fill) = fill {
        rect.fill = color(fill);
        rect.has_fill = 1;
    }
    if let Some(stroke) = stroke {
        rect.stroke = color(stroke);
        rect.has_stroke = 1;
        rect.stroke_width = stroke_width.unwrap_or(1.0);
    }
}

fn color(value: PaintColor) -> abi::AbiColor {
    match value {
        PaintColor::Rgba { rgba: [r, g, b, a] } => abi::AbiColor::rgba(r, g, b, a),
        PaintColor::Token { name } => abi::AbiColor::token(token_code(name)),
    }
}

fn token_code(name: ColorToken) -> u32 {
    match name {
        ColorToken::Text => abi::TOKEN_TEXT,
        ColorToken::TextSecondary => abi::TOKEN_TEXT_SECONDARY,
        ColorToken::TextMuted => abi::TOKEN_TEXT_MUTED,
        ColorToken::Accent => abi::TOKEN_ACCENT,
        ColorToken::AccentSoft => abi::TOKEN_ACCENT_SOFT,
        ColorToken::Danger => abi::TOKEN_DANGER,
        ColorToken::DangerSoft => abi::TOKEN_DANGER_SOFT,
        ColorToken::Success => abi::TOKEN_SUCCESS,
        ColorToken::SuccessSoft => abi::TOKEN_SUCCESS_SOFT,
        ColorToken::Warning => abi::TOKEN_WARNING,
        ColorToken::WarningSoft => abi::TOKEN_WARNING_SOFT,
        ColorToken::Surface => abi::TOKEN_SURFACE,
        ColorToken::SurfaceMuted => abi::TOKEN_SURFACE_MUTED,
        ColorToken::SurfaceSunken => abi::TOKEN_SURFACE_SUNKEN,
        ColorToken::Hover => abi::TOKEN_HOVER,
        ColorToken::Border => abi::TOKEN_BORDER,
        ColorToken::BorderStrong => abi::TOKEN_BORDER_STRONG,
        ColorToken::Transparent => abi::TOKEN_TRANSPARENT,
    }
}

fn weight_code(weight: Option<TextWeight>) -> u32 {
    match weight {
        Some(TextWeight::Medium) => abi::WEIGHT_MEDIUM,
        Some(TextWeight::SemiBold) => abi::WEIGHT_SEMI_BOLD,
        _ => abi::WEIGHT_NORMAL,
    }
}

fn family_code(family: Option<TextFamily>) -> u32 {
    match family {
        Some(TextFamily::Mono) => abi::FAMILY_MONO,
        _ => abi::FAMILY_UI,
    }
}

fn align_code(align: Option<TextAlign>) -> u32 {
    match align {
        Some(TextAlign::Center) => abi::ALIGN_CENTER,
        Some(TextAlign::End) => abi::ALIGN_END,
        _ => abi::ALIGN_START,
    }
}

fn fit_code(fit: Option<ImageFit>) -> u32 {
    match fit {
        Some(ImageFit::Contain) => abi::FIT_CONTAIN,
        Some(ImageFit::Cover) => abi::FIT_COVER,
        _ => abi::FIT_FILL,
    }
}

fn cursor_code(cursor: HitCursor) -> u32 {
    match cursor {
        HitCursor::Default => abi::CURSOR_DEFAULT,
        HitCursor::Pointer => abi::CURSOR_POINTER,
        HitCursor::Text => abi::CURSOR_TEXT,
        HitCursor::Grab => abi::CURSOR_GRAB,
        HitCursor::Grabbing => abi::CURSOR_GRABBING,
        HitCursor::Crosshair => abi::CURSOR_CROSSHAIR,
        HitCursor::NotAllowed => abi::CURSOR_NOT_ALLOWED,
        HitCursor::ResizeHorizontal => abi::CURSOR_RESIZE_H,
        HitCursor::ResizeVertical => abi::CURSOR_RESIZE_V,
    }
}

fn image_source(source: &ImageSource) -> (u32, String, bool) {
    match source {
        ImageSource::Private { path } => (abi::IMAGE_PRIVATE, path.clone(), false),
        ImageSource::Vault { path } => (abi::IMAGE_VAULT, path.clone(), false),
        ImageSource::Url { url, local } => (abi::IMAGE_URL, url.clone(), local.unwrap_or(false)),
        ImageSource::File { path } => (abi::IMAGE_FILE, path.clone(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::HitRegion;
    use crate::protocol::paint::PaintPoint;

    fn surface_frame() -> SurfaceFrame {
        SurfaceFrame {
            commands: vec![
                DrawCommand::Line {
                    x1: 0.0,
                    y1: 1.0,
                    x2: 2.0,
                    y2: 3.0,
                    stroke: PaintColor::Token { name: ColorToken::Border },
                    width: 1.5,
                },
                DrawCommand::Rect {
                    x: 4.0,
                    y: 5.0,
                    w: 6.0,
                    h: 7.0,
                    fill: Some(PaintColor::Rgba { rgba: [1, 2, 3, 4] }),
                    stroke: None,
                    stroke_width: None,
                    corner_radius: Some(2.0),
                    shadow: None,
                },
                DrawCommand::Path {
                    points: vec![PaintPoint { x: 1.0, y: 2.0 }, PaintPoint { x: 3.0, y: 4.0 }],
                    closed: true,
                    fill: None,
                    stroke: Some(PaintColor::Token { name: ColorToken::Accent }),
                    stroke_width: Some(2.0),
                },
                DrawCommand::Circle {
                    cx: 10.0,
                    cy: 11.0,
                    r: 12.0,
                    fill: Some(PaintColor::Token { name: ColorToken::Surface }),
                    stroke: None,
                    stroke_width: None,
                },
                DrawCommand::Text {
                    x: 2.0,
                    y: 3.0,
                    content: "hello".into(),
                    size: 14.0,
                    color: PaintColor::Token { name: ColorToken::Text },
                    weight: Some(TextWeight::SemiBold),
                    family: Some(TextFamily::Mono),
                    max_width: Some(80.0),
                    align: Some(TextAlign::Center),
                },
                DrawCommand::Image {
                    source: ImageSource::Vault { path: "note.png".into() },
                    x: 0.0,
                    y: 0.0,
                    w: 16.0,
                    h: 16.0,
                    fit: Some(ImageFit::Contain),
                },
            ],
            hits: vec![HitRegion {
                id: "button".into(),
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
                cursor: Some(HitCursor::Pointer),
            }],
            ime: Some(FrameRect { x: 1.0, y: 2.0, w: 3.0, h: 18.0 }),
            keys: true,
            fps: Some(30),
        }
    }

    fn str_of(value: abi::AbiStr) -> String {
        if value.is_empty() {
            return String::new();
        }
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(value.data, value.len) })
            .into_owned()
    }

    #[test]
    fn frame_buffer_covers_every_command_and_keeps_strings_alive() {
        let frame = surface_frame();
        let buffer = FrameBuffer::new(&frame);
        let raw = buffer.raw();
        assert_eq!(raw.commands_len, 6);
        assert_eq!(raw.keys, 1);
        assert_eq!(raw.fps, 30);
        assert_eq!(raw.has_ime, 1);
        let commands = unsafe { std::slice::from_raw_parts(raw.commands, raw.commands_len) };
        assert_eq!(commands[0].kind, abi::CMD_LINE);
        assert_eq!(commands[0].line_x2, 2.0);
        assert_eq!(commands[0].rect.stroke.kind, abi::COLOR_TOKEN);
        assert_eq!(commands[0].rect.stroke.token, abi::TOKEN_BORDER);
        assert_eq!(commands[1].kind, abi::CMD_RECT);
        assert_eq!(commands[1].rect.has_fill, 1);
        assert_eq!(commands[1].rect.fill.r, 1);
        assert_eq!(commands[1].rect.radius, 2.0);
        assert_eq!(commands[2].kind, abi::CMD_PATH);
        assert_eq!(commands[2].points_len, 2);
        assert_eq!(commands[2].closed, 1);
        assert_eq!(commands[2].rect.stroke_width, 2.0);
        assert_eq!(commands[3].kind, abi::CMD_CIRCLE);
        assert_eq!(commands[3].circle_r, 12.0);
        assert_eq!(commands[4].kind, abi::CMD_TEXT);
        assert_eq!(str_of(commands[4].text), "hello");
        assert_eq!(commands[4].text_weight, abi::WEIGHT_SEMI_BOLD);
        assert_eq!(commands[4].text_family, abi::FAMILY_MONO);
        assert_eq!(commands[4].text_align, abi::ALIGN_CENTER);
        assert_eq!(commands[4].text_max_width, 80.0);
        assert_eq!(commands[5].kind, abi::CMD_IMAGE);
        assert_eq!(commands[5].image_source, abi::IMAGE_VAULT);
        assert_eq!(str_of(commands[5].image_path), "note.png");
        let hits = unsafe { std::slice::from_raw_parts(raw.hits, raw.hits_len) };
        assert_eq!(str_of(hits[0].id), "button");
        assert_eq!(hits[0].cursor, abi::CURSOR_POINTER);
    }

    fn empty_input(kind: u32) -> abi::AbiInput {
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
            pressed: 0,
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

    #[test]
    fn abi_input_maps_named_and_character_keys() {
        let text = String::from("汉");
        let mut input = empty_input(abi::INPUT_KEY);
        input.key = abi::KEY_CHARACTER;
        input.text = abi::AbiStr::new(&text);
        input.pressed = 1;
        input.modifiers = abi::MOD_CONTROL | abi::MOD_SHIFT;
        match input_from_abi(&input).unwrap() {
            FrameInput::Key { key, action, modifiers, text: payload } => {
                assert_eq!(key, Key::Character { text: "汉".into() });
                assert_eq!(action, KeyAction::Press);
                assert!(modifiers.control && modifiers.shift);
                assert_eq!(payload.as_deref(), Some("汉"));
            }
            other => panic!("应是按键输入：{other:?}"),
        }

        let mut named = empty_input(abi::INPUT_KEY);
        named.key = abi::KEY_ARROW_LEFT;
        match input_from_abi(&named).unwrap() {
            FrameInput::Key { key, action, .. } => {
                assert_eq!(key, Key::Named { name: NamedKey::ArrowLeft });
                assert_eq!(action, KeyAction::Release);
            }
            other => panic!("应是按键输入：{other:?}"),
        }
    }

    #[test]
    fn abi_input_maps_text_resize_and_unknown() {
        let composing = String::from("nihao");
        let mut input = empty_input(abi::INPUT_TEXT);
        input.text = abi::AbiStr::new(&composing);
        input.composing = 1;
        input.has_caret = 1;
        input.caret = 5;
        input.has_candidates = 1;
        input.candidates = abi::AbiRect::filled(1.0, 2.0, 30.0, 18.0, 0.0, abi::AbiColor::TRANSPARENT);
        match input_from_abi(&input).unwrap() {
            FrameInput::Text { text, composing, caret, candidates } => {
                assert_eq!(text, "nihao");
                assert!(composing);
                assert_eq!(caret, Some(5));
                assert_eq!(candidates, Some(FrameRect { x: 1.0, y: 2.0, w: 30.0, h: 18.0 }));
            }
            other => panic!("应是文本输入：{other:?}"),
        }

        let mut resize = empty_input(abi::INPUT_RESIZE);
        resize.width = 640.0;
        resize.height = 480.0;
        assert_eq!(
            input_from_abi(&resize),
            Some(FrameInput::Resize { width: 640.0, height: 480.0 })
        );
        assert!(input_from_abi(&empty_input(9999)).is_none());
    }
}
