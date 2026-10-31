//! 同进程插件宿主：加载动态库、校验 ABI、转发帧 / 输入 / 服务调用。
//! 边界只走 `atelyx-plugin-abi` 的 C 布局值；宿主在回调返回前复制所有借用字符串与帧。

use std::ffi::{c_void, OsStr};
use std::sync::Arc;

use atelyx_plugin_abi as abi;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub kind: u32,
    pub rect: Rect,
    pub text: String,
    pub text_size: f32,
    pub text_color: Color,
    pub text_weight: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: String,
    pub rect: Rect,
    pub cursor: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub commands: Vec<Command>,
    pub hits: Vec<Hit>,
    pub width: f32,
    pub height: f32,
    pub ime: Option<Rect>,
    pub fps: u32,
}

impl Frame {
    /// 复制一帧；调用返回后插件可立即复用指令与字符串内存。
    ///
    /// # Safety
    /// `frame` 内的指针必须指向本进程内当前有效的指令 / 命中区 / 字符串；本函数在返回前复制全部内容。
    pub unsafe fn from_abi(frame: &abi::AbiFrame) -> Self {
        let commands = if frame.commands.is_null() || frame.commands_len == 0 {
            Vec::new()
        } else {
            let raw = unsafe { std::slice::from_raw_parts(frame.commands, frame.commands_len) };
            raw.iter()
                .map(|command| Command {
                    kind: command.kind,
                    rect: rect(command.rect),
                    text: copy_str(command.text),
                    text_size: command.text_size,
                    text_color: color(command.text_color),
                    text_weight: command.text_weight,
                })
                .collect()
        };
        let hits = if frame.hits.is_null() || frame.hits_len == 0 {
            Vec::new()
        } else {
            let raw = unsafe { std::slice::from_raw_parts(frame.hits, frame.hits_len) };
            raw.iter()
                .map(|hit| Hit { id: copy_str(hit.id), rect: rect(hit.rect), cursor: hit.cursor })
                .collect()
        };
        Self {
            commands,
            hits,
            width: frame.width,
            height: frame.height,
            ime: (frame.has_ime != 0).then(|| rect(frame.ime)),
            fps: frame.fps,
        }
    }
}

fn color(value: abi::AbiColor) -> Color {
    Color { r: value.r, g: value.g, b: value.b, a: value.a }
}

fn rect(value: abi::AbiRect) -> Rect {
    Rect {
        x: value.x,
        y: value.y,
        w: value.w,
        h: value.h,
        radius: value.radius,
        fill: (value.has_fill != 0).then(|| color(value.fill)),
        stroke: (value.has_stroke != 0).then(|| color(value.stroke)),
    }
}

/// 复制借用字符串；空指针 / 零长度按空串处理。
pub fn copy_str(value: abi::AbiStr) -> String {
    if value.data.is_null() || value.len == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(value.data, value.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// 宿主需要实现的全部回调；插件可在自己的线程上调用，因此实现必须 `Send + Sync`，且不得 panic。
pub trait Host: Send + Sync {
    fn submit_frame(&self, surface: &str, frame: Frame);
    fn request_redraw(&self, surface: &str);
    fn log(&self, level: u32, message: &str);
    /// 宿主必须恰好调用一次 `call.reply`（可异步、可从任意线程；异步时实现方持有 `call`）。
    /// 实例销毁前必须完成全部在途回执。
    fn call_service(&self, call: ServiceCall);
    fn subscribe(&self, name: &str);
}

/// 一次插件 → 宿主的服务调用；回执走插件给的函数指针。
pub struct ServiceCall {
    pub request_id: u64,
    pub service: String,
    pub method: String,
    pub params: String,
    reply: Option<Reply>,
}

impl ServiceCall {
    /// 调用方没给回执函数时什么都不做。
    pub fn reply(self, ok: bool, payload: &str) {
        if let Some(reply) = self.reply {
            reply.reply(ok, payload);
        }
    }
}

struct Reply {
    function: abi::ServiceReplyFn,
    ctx: *mut c_void,
    request_id: u64,
}

unsafe impl Send for Reply {}

impl Reply {
    fn reply(self, ok: bool, payload: &str) {
        unsafe {
            (self.function)(self.ctx, self.request_id, ok as u8, abi::AbiStr::new(payload))
        }
    }
}

/// `api.ctx` 指向自身，故实例必须装箱后再取地址；装箱后地址稳定，插件可保存函数表指针。
struct HostCtx {
    host: Arc<dyn Host>,
    api: abi::HostApi,
}

pub struct PluginInstance {
    lib: libloading::Library,
    api: abi::PluginApi,
    _host: Box<HostCtx>,
    handle: *mut c_void,
    destroyed: bool,
}

impl PluginInstance {
    /// 加载并创建插件实例；ABI 版本必须严格相等。
    pub fn load(path: impl AsRef<OsStr>, host: Arc<dyn Host>) -> Result<Self, String> {
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|error| format!("加载动态库失败：{error}"))?;
        let api = {
            let entry = unsafe { lib.get::<abi::PluginApiEntry>(abi::PLUGIN_API_SYMBOL) }
                .map_err(|error| format!("找不到插件入口符号：{error}"))?;
            let pointer = unsafe { entry() };
            if pointer.is_null() {
                return Err("插件入口返回空指针".into());
            }
            unsafe { *pointer }
        };
        check_abi(api.abi_version)?;
        let mut host = Box::new(HostCtx {
            host,
            api: abi::HostApi {
                abi_version: abi::ABI_VERSION,
                ctx: std::ptr::null_mut(),
                submit_frame: host_submit_frame,
                request_redraw: host_request_redraw,
                log: host_log,
                call_service: host_call_service,
                subscribe: host_subscribe,
            },
        });
        let ctx = &*host as *const HostCtx as *mut c_void;
        host.api.ctx = ctx;
        let handle = unsafe { (api.create)(&host.api) };
        if handle.is_null() {
            return Err("插件 create 返回空句柄（ABI 版本不符或初始化失败）".into());
        }
        Ok(Self { lib, api, _host: host, handle, destroyed: false })
    }

    pub fn attach(&self, surface: &str, width: f32, height: f32, kind: u32) {
        unsafe { (self.api.on_attach)(self.handle, abi::AbiStr::new(surface), width, height, kind) }
    }

    pub fn detach(&self, surface: &str) {
        unsafe { (self.api.on_detach)(self.handle, abi::AbiStr::new(surface)) }
    }

    pub fn input(&self, surface: &str, input: &abi::AbiInput) {
        unsafe { (self.api.on_input)(self.handle, abi::AbiStr::new(surface), input) }
    }

    pub fn tick(&self, surface: &str) {
        unsafe { (self.api.on_tick)(self.handle, abi::AbiStr::new(surface)) }
    }

    pub fn event(&self, name: &str, payload: &str) {
        unsafe {
            (self.api.on_event)(
                self.handle,
                abi::AbiStr::new(name),
                abi::AbiStr::new(payload),
            )
        }
    }

    /// 结束插件实例；调用后不再有方法可用（句柄随之失效）。
    pub fn destroy(mut self) {
        self.teardown();
    }

    fn teardown(&mut self) {
        if !self.destroyed {
            unsafe { (self.api.destroy)(self.handle) };
            self.destroyed = true;
        }
    }
}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        self.teardown();
        // 库必须活到 destroy 返回；字段随后按声明序释放。
        let _ = &self.lib;
    }
}

/// ABI 版本必须严格相等；宽容匹配会让旧插件带未知语义跑起来。
pub fn check_abi(version: u32) -> Result<(), String> {
    if version == abi::ABI_VERSION {
        Ok(())
    } else {
        Err(format!("ABI 版本不符：插件 {version}，宿主 {}", abi::ABI_VERSION))
    }
}

unsafe extern "C" fn host_submit_frame(
    ctx: *mut c_void,
    surface: abi::AbiStr,
    frame: *const abi::AbiFrame,
) {
    if ctx.is_null() || frame.is_null() {
        return;
    }
    let ctx = unsafe { &*(ctx as *const HostCtx) };
    let frame = unsafe { Frame::from_abi(&*frame) };
    ctx.host.submit_frame(&copy_str(surface), frame);
}

unsafe extern "C" fn host_request_redraw(ctx: *mut c_void, surface: abi::AbiStr) {
    if ctx.is_null() {
        return;
    }
    let ctx = unsafe { &*(ctx as *const HostCtx) };
    ctx.host.request_redraw(&copy_str(surface));
}

unsafe extern "C" fn host_log(ctx: *mut c_void, level: u32, message: abi::AbiStr) {
    if ctx.is_null() {
        return;
    }
    let ctx = unsafe { &*(ctx as *const HostCtx) };
    ctx.host.log(level, &copy_str(message));
}

unsafe extern "C" fn host_call_service(
    ctx: *mut c_void,
    request_id: u64,
    service: abi::AbiStr,
    method: abi::AbiStr,
    params: abi::AbiStr,
    reply: Option<abi::ServiceReplyFn>,
    reply_ctx: *mut c_void,
) {
    if ctx.is_null() {
        return;
    }
    let ctx = unsafe { &*(ctx as *const HostCtx) };
    let Some(function) = reply else {
        ctx.host.log(abi::LOG_ERROR, "服务调用没有回执函数");
        return;
    };
    ctx.host.call_service(ServiceCall {
        request_id,
        service: copy_str(service),
        method: copy_str(method),
        params: copy_str(params),
        reply: Some(Reply { function, ctx: reply_ctx, request_id }),
    });
}

unsafe extern "C" fn host_subscribe(ctx: *mut c_void, name: abi::AbiStr) {
    if ctx.is_null() {
        return;
    }
    let ctx = unsafe { &*(ctx as *const HostCtx) };
    ctx.host.subscribe(&copy_str(name));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_must_match_exactly() {
        assert!(check_abi(abi::ABI_VERSION).is_ok());
        assert!(check_abi(abi::ABI_VERSION + 1).is_err());
    }

    #[test]
    fn frame_copy_owns_strings_after_callback() {
        let text = String::from("hello");
        let id = String::from("button");
        let commands = [abi::AbiCommand {
            kind: abi::CMD_TEXT,
            rect: abi::AbiRect::empty_at(1.0, 2.0),
            text: abi::AbiStr::new(&text),
            text_size: 14.0,
            text_color: abi::AbiColor::rgba(255, 255, 255, 255),
            text_weight: 400,
        }];
        let hits = [abi::AbiHit {
            id: abi::AbiStr::new(&id),
            rect: abi::AbiRect::filled(0.0, 0.0, 10.0, 10.0, 0.0, abi::AbiColor::rgba(0, 0, 0, 0)),
            cursor: abi::CURSOR_POINTER,
        }];
        let frame = abi::AbiFrame {
            commands: commands.as_ptr(),
            commands_len: commands.len(),
            hits: hits.as_ptr(),
            hits_len: hits.len(),
            width: 100.0,
            height: 80.0,
            has_ime: 0,
            ime: abi::AbiRect::empty_at(0.0, 0.0),
            fps: 0,
        };
        let owned = unsafe { Frame::from_abi(&frame) };
        drop(text);
        drop(id);
        let _ = (&commands, &hits);
        assert_eq!(owned.commands[0].text, "hello");
        assert_eq!(owned.hits[0].id, "button");
        assert_eq!(owned.width, 100.0);
    }
}
