//! 同进程插件 ABI：跨动态库只传 C 布局值与函数指针，不传 Rust / iced 类型。
//! `AbiStr` 只在当前调用内有效，收到方必须在返回前复制；宿主与插件各持一张函数表。
//! 宿主对同一 handle 的入口调用串行发生；`destroy` 返回后不再有任何调用。

use core::ffi::c_void;

pub const ABI_VERSION: u32 = 1;
/// 插件导出符号（含结尾 NUL，供 `libloading` 按字节查找）。
pub const PLUGIN_API_SYMBOL: &[u8] = b"atelyx_plugin_api\0";

pub const CMD_RECT: u32 = 1;
pub const CMD_TEXT: u32 = 2;

pub const CURSOR_DEFAULT: u32 = 0;
pub const CURSOR_POINTER: u32 = 1;
pub const CURSOR_TEXT: u32 = 2;
pub const CURSOR_GRAB: u32 = 3;
pub const CURSOR_GRABBING: u32 = 4;
pub const CURSOR_RESIZE_H: u32 = 5;
pub const CURSOR_RESIZE_V: u32 = 6;
pub const CURSOR_CROSSHAIR: u32 = 7;
pub const CURSOR_NOT_ALLOWED: u32 = 8;

pub const SURFACE_PANEL: u32 = 1;
pub const SURFACE_WINDOW: u32 = 2;
pub const SURFACE_TAKEOVER_PAGE: u32 = 3;
pub const SURFACE_TAKEOVER_FULL: u32 = 4;
pub const SURFACE_DOC: u32 = 5;
pub const SURFACE_SETTINGS: u32 = 6;

pub const INPUT_POINTER_DOWN: u32 = 1;
pub const INPUT_POINTER_UP: u32 = 2;
pub const INPUT_POINTER_MOVE: u32 = 3;
pub const INPUT_POINTER_LEAVE: u32 = 4;
pub const INPUT_DOUBLE_CLICK: u32 = 5;
pub const INPUT_WHEEL: u32 = 6;
pub const INPUT_KEY: u32 = 7;
pub const INPUT_TEXT: u32 = 8;
pub const INPUT_FOCUS: u32 = 9;
pub const INPUT_RESIZE: u32 = 10;
pub const INPUT_DROP: u32 = 11;

pub const LOG_INFO: u32 = 1;
pub const LOG_ERROR: u32 = 2;

/// 借用字符串：只在当前调用内有效。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiStr {
    pub data: *const u8,
    pub len: usize,
}

impl AbiStr {
    pub const EMPTY: Self = Self { data: core::ptr::null(), len: 0 };

    pub fn new(value: &str) -> Self {
        Self { data: value.as_ptr(), len: value.len() }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl AbiColor {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub fill: AbiColor,
    pub has_fill: u8,
    pub stroke: AbiColor,
    pub has_stroke: u8,
}

impl AbiRect {
    pub const fn filled(x: f32, y: f32, w: f32, h: f32, radius: f32, fill: AbiColor) -> Self {
        Self {
            x,
            y,
            w,
            h,
            radius,
            fill,
            has_fill: 1,
            stroke: AbiColor::rgba(0, 0, 0, 0),
            has_stroke: 0,
        }
    }

    pub const fn empty_at(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            w: 0.0,
            h: 0.0,
            radius: 0.0,
            fill: AbiColor::rgba(0, 0, 0, 0),
            has_fill: 0,
            stroke: AbiColor::rgba(0, 0, 0, 0),
            has_stroke: 0,
        }
    }
}

/// 一条绘制指令；文字指令用 `rect.x/y` 定位。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiCommand {
    pub kind: u32,
    pub rect: AbiRect,
    pub text: AbiStr,
    pub text_size: f32,
    pub text_color: AbiColor,
    pub text_weight: u32,
}

/// 一个命中区；后声明的压住先声明的（宿主按声明序命中）。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiHit {
    pub id: AbiStr,
    pub rect: AbiRect,
    pub cursor: u32,
}

/// 整面帧；宿主同步复制，返回后插件可复用 / 释放内存。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiFrame {
    pub commands: *const AbiCommand,
    pub commands_len: usize,
    pub hits: *const AbiHit,
    pub hits_len: usize,
    pub width: f32,
    pub height: f32,
    pub has_ime: u8,
    pub ime: AbiRect,
    pub fps: u32,
}

/// 一条原始输入；坐标面本地，滚轮增量统一为像素。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiInput {
    pub kind: u32,
    pub x: f32,
    pub y: f32,
    pub button: u32,
    pub key: u32,
    pub text: AbiStr,
    pub pressed: u8,
    pub modifiers: u32,
}

/// 服务调用回执：宿主必须恰好调用一次，可从任意宿主线程调用；`payload` 只在调用内有效。
/// `None`（空指针）表示调用方无从回执，宿主只记日志不回调。
pub type ServiceReplyFn =
    unsafe extern "C" fn(ctx: *mut c_void, request_id: u64, ok: u8, payload: AbiStr);

/// 宿主提供给插件的函数表；插件只经它回调宿主，可从任意插件线程调用。
/// 指针在实例生命周期内有效（`create` 与后续入口拿到的同一份）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub abi_version: u32,
    pub ctx: *mut c_void,
    pub submit_frame: unsafe extern "C" fn(*mut c_void, AbiStr, *const AbiFrame),
    pub request_redraw: unsafe extern "C" fn(*mut c_void, AbiStr),
    pub log: unsafe extern "C" fn(*mut c_void, u32, AbiStr),
    pub call_service: unsafe extern "C" fn(
        *mut c_void,
        u64,
        AbiStr,
        AbiStr,
        AbiStr,
        Option<ServiceReplyFn>,
        *mut c_void,
    ),
    pub subscribe: unsafe extern "C" fn(*mut c_void, AbiStr),
}

/// 插件导出的函数表；宿主加载后先校验 `abi_version`。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginApi {
    pub abi_version: u32,
    /// 成功返回实例句柄，失败回空指针；`HostApi` 指针可保存到实例结束。
    pub create: unsafe extern "C" fn(*const HostApi) -> *mut c_void,
    /// 必须停掉插件自建线程后再返回；返回后宿主不再调用任何入口。
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub on_attach: unsafe extern "C" fn(*mut c_void, AbiStr, f32, f32, u32),
    pub on_detach: unsafe extern "C" fn(*mut c_void, AbiStr),
    pub on_input: unsafe extern "C" fn(*mut c_void, AbiStr, *const AbiInput),
    pub on_tick: unsafe extern "C" fn(*mut c_void, AbiStr),
    pub on_event: unsafe extern "C" fn(*mut c_void, AbiStr, AbiStr),
}

pub type PluginApiEntry = unsafe extern "C" fn() -> *const PluginApi;
