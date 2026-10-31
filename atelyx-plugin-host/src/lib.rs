//! 同进程插件宿主：加载动态库、校验 ABI、转发帧 / 输入 / 服务调用。
//! 边界只走 `atelyx-plugin-abi` 的 C 布局值；宿主在回调返回前复制所有借用字符串与帧。
//! 动态库对象与插件实例分离：停用只销毁实例，库由调用方保留到进程退出。

use std::ffi::{c_void, OsStr};
use std::sync::Arc;

use atelyx_plugin_abi as abi;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    Rgba { r: u8, g: u8, b: u8, a: u8 },
    Token(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub color: Color,
}

/// 通用矩形：命中区与输入法光标只用坐标，填充 / 描边为 `None`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub stroke_width: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImageSource {
    Private(String),
    Vault(String),
    Url { url: String, local: bool },
    File(String),
}

/// 一条已复制到宿主内存的绘制指令。
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Line { x1: f32, y1: f32, x2: f32, y2: f32, stroke: Color, width: f32 },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<Color>,
        stroke: Option<Color>,
        stroke_width: f32,
        corner_radius: Option<f32>,
        shadow: Option<Shadow>,
    },
    Path {
        points: Vec<Point>,
        closed: bool,
        fill: Option<Color>,
        stroke: Option<Color>,
        stroke_width: f32,
    },
    Circle { cx: f32, cy: f32, r: f32, fill: Option<Color>, stroke: Option<Color>, stroke_width: f32 },
    Text {
        x: f32,
        y: f32,
        content: String,
        size: f32,
        color: Color,
        weight: u32,
        family: u32,
        max_width: Option<f32>,
        align: u32,
    },
    Image { source: ImageSource, x: f32, y: f32, w: f32, h: f32, fit: u32 },
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
    pub ime: Option<Rect>,
    pub keys: bool,
    pub fps: u32,
}

impl Frame {
    /// 复制一帧；调用返回后插件可立即复用指令与字符串内存。
    ///
    /// # Safety
    /// `frame` 内的指针必须指向本进程内当前有效的指令 / 命中区 / 字符串；本函数在返回前复制全部内容。
    pub unsafe fn try_from_abi(frame: &abi::AbiFrame) -> Result<Self, String> {
        let commands = if frame.commands.is_null() || frame.commands_len == 0 {
            Vec::new()
        } else {
            let raw = unsafe { std::slice::from_raw_parts(frame.commands, frame.commands_len) };
            raw.iter()
                .map(|raw_command| unsafe { command(raw_command) })
                .collect::<Result<_, _>>()?
        };
        let hits = if frame.hits.is_null() || frame.hits_len == 0 {
            Vec::new()
        } else {
            let raw = unsafe { std::slice::from_raw_parts(frame.hits, frame.hits_len) };
            raw.iter()
                .map(|hit| Hit { id: copy_str(hit.id), rect: rect(hit.rect), cursor: hit.cursor })
                .collect()
        };
        Ok(Self {
            commands,
            hits,
            ime: (frame.has_ime != 0).then(|| rect(frame.ime)),
            keys: frame.keys != 0,
            fps: frame.fps,
        })
    }
}

unsafe fn command(value: &abi::AbiCommand) -> Result<Command, String> {
    Ok(match value.kind {
        abi::CMD_LINE => Command::Line {
            x1: value.rect.x,
            y1: value.rect.y,
            x2: value.line_x2,
            y2: value.line_y2,
            stroke: color(value.rect.stroke),
            width: stroke_width(value.rect.stroke_width),
        },
        abi::CMD_RECT => Command::Rect {
            x: value.rect.x,
            y: value.rect.y,
            w: value.rect.w,
            h: value.rect.h,
            fill: (value.rect.has_fill != 0).then(|| color(value.rect.fill)),
            stroke: (value.rect.has_stroke != 0).then(|| color(value.rect.stroke)),
            stroke_width: stroke_width(value.rect.stroke_width),
            corner_radius: (value.rect.radius > 0.0).then_some(value.rect.radius),
            shadow: (value.shadow.enabled != 0).then(|| Shadow {
                offset_x: value.shadow.offset_x,
                offset_y: value.shadow.offset_y,
                blur: value.shadow.blur,
                color: color(value.shadow.color),
            }),
        },
        abi::CMD_PATH => {
            let points = if value.points.is_null() || value.points_len == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(value.points, value.points_len) }
                    .iter()
                    .map(|point| Point { x: point.x, y: point.y })
                    .collect()
            };
            Command::Path {
                points,
                closed: value.closed != 0,
                fill: (value.rect.has_fill != 0).then(|| color(value.rect.fill)),
                stroke: (value.rect.has_stroke != 0).then(|| color(value.rect.stroke)),
                stroke_width: stroke_width(value.rect.stroke_width),
            }
        }
        abi::CMD_CIRCLE => Command::Circle {
            cx: value.circle_cx,
            cy: value.circle_cy,
            r: value.circle_r,
            fill: (value.rect.has_fill != 0).then(|| color(value.rect.fill)),
            stroke: (value.rect.has_stroke != 0).then(|| color(value.rect.stroke)),
            stroke_width: stroke_width(value.rect.stroke_width),
        },
        abi::CMD_TEXT => Command::Text {
            x: value.rect.x,
            y: value.rect.y,
            content: copy_str(value.text),
            size: value.text_size,
            color: color(value.text_color),
            weight: value.text_weight,
            family: value.text_family,
            max_width: (value.text_max_width > 0.0).then_some(value.text_max_width),
            align: value.text_align,
        },
        abi::CMD_IMAGE => Command::Image {
            source: image_source(value.image_source, value.image_path, value.image_local != 0)?,
            x: value.rect.x,
            y: value.rect.y,
            w: value.rect.w,
            h: value.rect.h,
            fit: value.image_fit,
        },
        other => return Err(format!("未知绘制指令：{other}")),
    })
}

fn image_source(kind: u32, path: abi::AbiStr, local: bool) -> Result<ImageSource, String> {
    let path = copy_str(path);
    Ok(match kind {
        abi::IMAGE_PRIVATE => ImageSource::Private(path),
        abi::IMAGE_VAULT => ImageSource::Vault(path),
        abi::IMAGE_URL => ImageSource::Url { url: path, local },
        abi::IMAGE_FILE => ImageSource::File(path),
        other => return Err(format!("未知图片来源：{other}")),
    })
}

fn color(value: abi::AbiColor) -> Color {
    if value.kind == abi::COLOR_TOKEN {
        Color::Token(value.token)
    } else {
        Color::Rgba { r: value.r, g: value.g, b: value.b, a: value.a }
    }
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
        stroke_width: stroke_width(value.stroke_width),
    }
}

/// 描边宽度缺省口径：未填或非正数按 1 个逻辑像素（与协议侧缺省一致）。
fn stroke_width(value: f32) -> f32 {
    if value > 0.0 {
        value
    } else {
        1.0
    }
}

/// 复制借用字符串；空指针 / 零长度按空串处理。
fn copy_str(value: abi::AbiStr) -> String {
    if value.is_empty() {
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
    fn call_host(&self, call: HostCall);
    fn subscribe(&self, name: &str);
    /// 回报一次宿主发起的服务调用；`call_id` 来自 `on_service_call`。
    fn reply_service(&self, call_id: u64, ok: bool, payload: &str);
}

/// 一次插件 → 宿主的调用；回执走插件给的函数指针。
pub struct HostCall {
    pub request_id: u64,
    pub method: String,
    pub params: String,
    reply: Option<Reply>,
}

impl HostCall {
    /// 调用方没给回执函数时什么都不做。
    pub fn reply(mut self, ok: bool, payload: &str) {
        if let Some(reply) = self.reply.take() {
            reply.reply(ok, payload);
        }
    }
}

impl Drop for HostCall {
    /// 宿主实现丢弃调用即视为未处理：以可读失败回执，插件的等待不会悬空。
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            reply.reply(false, "宿主未处理该调用");
        }
    }
}

struct Reply {
    function: abi::HostReplyFn,
    ctx: *mut c_void,
    request_id: u64,
}

unsafe impl Send for Reply {}

impl Reply {
    fn reply(self, ok: bool, payload: &str) {
        guarded(|| unsafe {
            (self.function)(self.ctx, self.request_id, ok as u8, abi::AbiStr::new(payload))
        });
    }
}

/// `api.ctx` 指向自身，故实例必须装箱后再取地址；装箱后地址稳定，插件可保存函数表指针。
struct HostCtx {
    host: Arc<dyn Host>,
    api: abi::HostApi,
}

/// 已加载的动态库：可重复创建实例；实例销毁后库仍可继续使用。
pub struct PluginLibrary {
    _lib: libloading::Library,
    api: abi::PluginApi,
}

impl PluginLibrary {
    /// 加载动态库并校验导出的 ABI 版本必须与宿主严格相等。
    pub fn load(path: impl AsRef<OsStr>) -> Result<Self, String> {
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
        Ok(Self { _lib: lib, api })
    }

    /// 创建一个插件实例；`host` 在实例销毁前必须保持有效。
    pub fn create(self: &Arc<Self>, host: Arc<dyn Host>) -> Result<PluginInstance, String> {
        let mut host = Box::new(HostCtx {
            host,
            api: abi::HostApi {
                abi_version: abi::ABI_VERSION,
                ctx: std::ptr::null_mut(),
                submit_frame: host_submit_frame,
                request_redraw: host_request_redraw,
                log: host_log,
                call_host: host_call_host,
                subscribe: host_subscribe,
                reply_service: host_reply_service,
            },
        });
        // 自指地址在装箱后稳定；用裸指针写入，避免先派生共享引用再改写同一分配
        let ctx = std::ptr::addr_of_mut!(*host).cast::<c_void>();
        unsafe { (*ctx.cast::<HostCtx>()).api.ctx = ctx };
        let api = unsafe { &(*ctx.cast::<HostCtx>()).api };
        let handle = unsafe { (self.api.create)(api) };
        if handle.is_null() {
            return Err("插件 create 返回空句柄（ABI 版本不符或初始化失败）".into());
        }
        Ok(PluginInstance { library: self.clone(), handle, _host: host, destroyed: false })
    }
}

pub struct PluginInstance {
    library: Arc<PluginLibrary>,
    handle: *mut c_void,
    _host: Box<HostCtx>,
    destroyed: bool,
}

// 句柄由插件导出、入口允许从任意线程调用；调用方仍须串行化对同一实例的入口调用。
unsafe impl Send for PluginInstance {}
// 多线程共享的只有不可变句柄与宿主回调表；插件侧状态由调用方的入口串行化保护。
unsafe impl Sync for PluginInstance {}

impl PluginInstance {
    pub fn attach(&self, surface: &str, kind: u32) {
        unsafe { (self.library.api.on_attach)(self.handle, abi::AbiStr::new(surface), kind) }
    }

    pub fn detach(&self, surface: &str) {
        unsafe { (self.library.api.on_detach)(self.handle, abi::AbiStr::new(surface)) }
    }

    /// # Safety
    /// `input` 内的字符串指针必须在本调用期间有效（由调用方持有的缓冲区保证）。
    pub unsafe fn input(&self, surface: &str, input: &abi::AbiInput) {
        unsafe { (self.library.api.on_input)(self.handle, abi::AbiStr::new(surface), input) }
    }

    pub fn tick(&self, surface: &str, frame: u64, delta_ms: u32) {
        unsafe {
            (self.library.api.on_tick)(self.handle, abi::AbiStr::new(surface), frame, delta_ms)
        }
    }

    pub fn event(&self, name: &str, payload: &str) {
        unsafe {
            (self.library.api.on_event)(
                self.handle,
                abi::AbiStr::new(name),
                abi::AbiStr::new(payload),
            )
        }
    }

    pub fn service_call(&self, call_id: u64, service: &str, method: &str, params: &str) {
        unsafe {
            (self.library.api.on_service_call)(
                self.handle,
                call_id,
                abi::AbiStr::new(service),
                abi::AbiStr::new(method),
                abi::AbiStr::new(params),
            )
        }
    }

    /// 结束插件实例；调用后不再有方法可用（句柄随之失效，库仍由 [`PluginLibrary`] 持有）。
    pub fn destroy(mut self) {
        self.teardown();
    }

    fn teardown(&mut self) {
        if !self.destroyed {
            unsafe { (self.library.api.destroy)(self.handle) };
            self.destroyed = true;
        }
    }
}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        self.teardown();
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

/// 宿主回调不得让 panic 越过 `extern "C"` 边界（那会直接 abort 整个应用）。
fn guarded(body: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
}

unsafe extern "C" fn host_submit_frame(
    ctx: *mut c_void,
    surface: abi::AbiStr,
    frame: *const abi::AbiFrame,
) {
    guarded(|| {
        if ctx.is_null() || frame.is_null() {
            return;
        }
        let ctx = unsafe { &*(ctx as *const HostCtx) };
        match unsafe { Frame::try_from_abi(&*frame) } {
            Ok(frame) => ctx.host.submit_frame(&copy_str(surface), frame),
            Err(error) => ctx.host.log(abi::LOG_ERROR, &format!("帧内容不合法，已丢弃：{error}")),
        }
    });
}

unsafe extern "C" fn host_request_redraw(ctx: *mut c_void, surface: abi::AbiStr) {
    guarded(|| {
        if ctx.is_null() {
            return;
        }
        let ctx = unsafe { &*(ctx as *const HostCtx) };
        ctx.host.request_redraw(&copy_str(surface));
    });
}

unsafe extern "C" fn host_log(ctx: *mut c_void, level: u32, message: abi::AbiStr) {
    guarded(|| {
        if ctx.is_null() {
            return;
        }
        let ctx = unsafe { &*(ctx as *const HostCtx) };
        ctx.host.log(level, &copy_str(message));
    });
}

unsafe extern "C" fn host_call_host(
    ctx: *mut c_void,
    request_id: u64,
    method: abi::AbiStr,
    params: abi::AbiStr,
    reply: Option<abi::HostReplyFn>,
    reply_ctx: *mut c_void,
) {
    guarded(|| {
        if ctx.is_null() {
            return;
        }
        let ctx = unsafe { &*(ctx as *const HostCtx) };
        let Some(function) = reply else {
            ctx.host.log(abi::LOG_ERROR, "宿主调用没有回执函数");
            return;
        };
        ctx.host.call_host(HostCall {
            request_id,
            method: copy_str(method),
            params: copy_str(params),
            reply: Some(Reply { function, ctx: reply_ctx, request_id }),
        });
    });
}

unsafe extern "C" fn host_subscribe(ctx: *mut c_void, name: abi::AbiStr) {
    guarded(|| {
        if ctx.is_null() {
            return;
        }
        let ctx = unsafe { &*(ctx as *const HostCtx) };
        ctx.host.subscribe(&copy_str(name));
    });
}

unsafe extern "C" fn host_reply_service(
    ctx: *mut c_void,
    call_id: u64,
    ok: u8,
    payload: abi::AbiStr,
) {
    guarded(|| {
        if ctx.is_null() {
            return;
        }
        let ctx = unsafe { &*(ctx as *const HostCtx) };
        ctx.host.reply_service(call_id, ok != 0, &copy_str(payload));
    });
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
    fn frame_copy_owns_strings_and_geometry() {
        let text = String::from("hello");
        let id = String::from("button");
        let path = String::from("note.md");
        let points = [abi::AbiPoint { x: 1.0, y: 2.0 }, abi::AbiPoint { x: 3.0, y: 4.0 }];
        let commands = [
            abi::AbiCommand {
                kind: abi::CMD_TEXT,
                rect: abi::AbiRect::empty_at(1.0, 2.0),
                shadow: abi::AbiShadow::NONE,
                line_x2: 0.0,
                line_y2: 0.0,
                circle_cx: 0.0,
                circle_cy: 0.0,
                circle_r: 0.0,
                points: std::ptr::null(),
                points_len: 0,
                closed: 0,
                text: abi::AbiStr::new(&text),
                text_size: 14.0,
                text_color: abi::AbiColor::token(abi::TOKEN_TEXT),
                text_weight: abi::WEIGHT_SEMI_BOLD,
                text_family: abi::FAMILY_UI,
                text_align: abi::ALIGN_CENTER,
                text_max_width: 120.0,
                image_source: 0,
                image_path: abi::AbiStr::EMPTY,
                image_local: 0,
                image_fit: abi::FIT_FILL,
            },
            abi::AbiCommand {
                kind: abi::CMD_PATH,
                rect: abi::AbiRect::filled(0.0, 0.0, 10.0, 10.0, 0.0, abi::AbiColor::TRANSPARENT),
                shadow: abi::AbiShadow::NONE,
                line_x2: 0.0,
                line_y2: 0.0,
                circle_cx: 0.0,
                circle_cy: 0.0,
                circle_r: 0.0,
                points: points.as_ptr(),
                points_len: points.len(),
                closed: 1,
                text: abi::AbiStr::EMPTY,
                text_size: 0.0,
                text_color: abi::AbiColor::TRANSPARENT,
                text_weight: 0,
                text_family: 0,
                text_align: 0,
                text_max_width: 0.0,
                image_source: 0,
                image_path: abi::AbiStr::EMPTY,
                image_local: 0,
                image_fit: 0,
            },
            abi::AbiCommand {
                kind: abi::CMD_IMAGE,
                rect: abi::AbiRect::filled(0.0, 0.0, 32.0, 32.0, 4.0, abi::AbiColor::TRANSPARENT),
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
                text_weight: 0,
                text_family: 0,
                text_align: 0,
                text_max_width: 0.0,
                image_source: abi::IMAGE_VAULT,
                image_path: abi::AbiStr::new(&path),
                image_local: 0,
                image_fit: abi::FIT_COVER,
            },
        ];
        let hits = [abi::AbiHit {
            id: abi::AbiStr::new(&id),
            rect: abi::AbiRect::filled(0.0, 0.0, 10.0, 10.0, 0.0, abi::AbiColor::TRANSPARENT),
            cursor: abi::CURSOR_POINTER,
        }];
        let frame = abi::AbiFrame {
            commands: commands.as_ptr(),
            commands_len: commands.len(),
            hits: hits.as_ptr(),
            hits_len: hits.len(),
            has_ime: 0,
            ime: abi::AbiRect::empty_at(0.0, 0.0),
            keys: 1,
            fps: 30,
        };
        let owned = unsafe { Frame::try_from_abi(&frame) }.unwrap();
        drop(text);
        drop(id);
        drop(path);
        let _ = (&commands, &hits, &points);
        assert_eq!(owned.commands.len(), 3);
        let Command::Text { content, color, max_width, align, .. } = &owned.commands[0] else {
            panic!("第一条应是文字");
        };
        assert_eq!(content, "hello");
        assert_eq!(*color, Color::Token(abi::TOKEN_TEXT));
        assert_eq!(*max_width, Some(120.0));
        assert_eq!(*align, abi::ALIGN_CENTER);
        let Command::Path { points, closed, .. } = &owned.commands[1] else {
            panic!("第二条应是路径");
        };
        assert_eq!(points.len(), 2);
        assert!(*closed);
        let Command::Image { source, fit, .. } = &owned.commands[2] else {
            panic!("第三条应是图片");
        };
        assert_eq!(*source, ImageSource::Vault("note.md".into()));
        assert_eq!(*fit, abi::FIT_COVER);
        assert_eq!(owned.hits[0].id, "button");
        assert!(owned.keys);
        assert_eq!(owned.fps, 30);
    }

    fn base(kind: u32) -> abi::AbiCommand {
        abi::AbiCommand {
            kind,
            rect: abi::AbiRect::empty_at(0.0, 0.0),
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

    #[test]
    fn frame_copy_covers_geometry_shadow_and_ime() {
        let mut line = base(abi::CMD_LINE);
        line.rect = abi::AbiRect::empty_at(1.0, 2.0);
        line.rect.stroke = abi::AbiColor::rgba(9, 8, 7, 255);
        line.rect.has_stroke = 1;
        line.rect.stroke_width = 2.0;
        line.line_x2 = 3.0;
        line.line_y2 = 4.0;
        let mut area = base(abi::CMD_RECT);
        area.rect = abi::AbiRect::filled(5.0, 6.0, 7.0, 8.0, 2.0, abi::AbiColor::rgba(1, 2, 3, 4));
        area.rect.stroke = abi::AbiColor::token(abi::TOKEN_BORDER);
        area.rect.has_stroke = 1;
        area.rect.stroke_width = 0.0;
        area.shadow = abi::AbiShadow {
            offset_x: 1.0,
            offset_y: 2.0,
            blur: 3.0,
            color: abi::AbiColor::rgba(0, 0, 0, 40),
            enabled: 1,
        };
        let mut circle = base(abi::CMD_CIRCLE);
        circle.circle_cx = 10.0;
        circle.circle_cy = 11.0;
        circle.circle_r = 12.0;
        circle.rect.has_fill = 1;
        circle.rect.fill = abi::AbiColor::token(abi::TOKEN_ACCENT);
        let commands = [line, area, circle];
        let frame = abi::AbiFrame {
            commands: commands.as_ptr(),
            commands_len: commands.len(),
            hits: std::ptr::null(),
            hits_len: 0,
            has_ime: 1,
            ime: abi::AbiRect::filled(4.0, 5.0, 6.0, 18.0, 0.0, abi::AbiColor::TRANSPARENT),
            keys: 0,
            fps: 0,
        };
        let owned = unsafe { Frame::try_from_abi(&frame) }.unwrap();
        let Command::Line { x1, y1, x2, y2, stroke, width } = &owned.commands[0] else {
            panic!("第一条应是线");
        };
        assert_eq!((*x1, *y1, *x2, *y2), (1.0, 2.0, 3.0, 4.0));
        assert_eq!(*stroke, Color::Rgba { r: 9, g: 8, b: 7, a: 255 });
        assert_eq!(*width, 2.0);
        let Command::Rect { stroke, stroke_width, shadow, .. } = &owned.commands[1] else {
            panic!("第二条应是矩形");
        };
        assert_eq!(*stroke, Some(Color::Token(abi::TOKEN_BORDER)));
        assert_eq!(*stroke_width, 1.0, "未填描边宽度按 1 处理");
        assert_eq!(*shadow, Some(Shadow {
            offset_x: 1.0,
            offset_y: 2.0,
            blur: 3.0,
            color: Color::Rgba { r: 0, g: 0, b: 0, a: 40 },
        }));
        let Command::Circle { cx, cy, r, fill, .. } = &owned.commands[2] else {
            panic!("第三条应是圆");
        };
        assert_eq!((*cx, *cy, *r), (10.0, 11.0, 12.0));
        assert_eq!(*fill, Some(Color::Token(abi::TOKEN_ACCENT)));
        assert_eq!(owned.ime.unwrap().h, 18.0);
    }

    #[test]
    fn frame_copy_rejects_unknown_commands_and_image_sources() {
        for (command, needle) in [
            (base(9999), "未知绘制指令"),
            (base(abi::CMD_IMAGE), "未知图片来源"),
        ] {
            let frame = abi::AbiFrame {
                commands: &command,
                commands_len: 1,
                hits: std::ptr::null(),
                hits_len: 0,
                has_ime: 0,
                ime: abi::AbiRect::empty_at(0.0, 0.0),
                keys: 0,
                fps: 0,
            };
            let error = unsafe { Frame::try_from_abi(&frame) }.unwrap_err();
            assert!(error.contains(needle), "{error}");
        }
    }
}
