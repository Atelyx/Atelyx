//! 同进程插件 ABI：跨动态库只传 C 布局值与函数指针，不传 Rust / iced 类型。
//! `AbiStr` 只在当前调用内有效，收到方必须在返回前复制；宿主与插件各持一张函数表。
//! 宿主对同一 handle 的入口调用串行发生；`destroy` 返回后不再有任何调用。

use core::ffi::c_void;

pub const ABI_VERSION: u32 = 1;
/// 插件导出符号（含结尾 NUL，供 `libloading` 按字节查找）。
pub const PLUGIN_API_SYMBOL: &[u8] = b"atelyx_plugin_api\0";

pub const CMD_LINE: u32 = 1;
pub const CMD_RECT: u32 = 2;
pub const CMD_PATH: u32 = 3;
pub const CMD_CIRCLE: u32 = 4;
pub const CMD_TEXT: u32 = 5;
pub const CMD_IMAGE: u32 = 6;

/// 颜色来源：`COLOR_RGBA` 读 r/g/b/a，`COLOR_TOKEN` 读 token（跟随宿主主题）。
pub const COLOR_RGBA: u32 = 0;
pub const COLOR_TOKEN: u32 = 1;

pub const TOKEN_TEXT: u32 = 1;
pub const TOKEN_TEXT_SECONDARY: u32 = 2;
pub const TOKEN_TEXT_MUTED: u32 = 3;
pub const TOKEN_ACCENT: u32 = 4;
pub const TOKEN_ACCENT_SOFT: u32 = 5;
pub const TOKEN_DANGER: u32 = 6;
pub const TOKEN_DANGER_SOFT: u32 = 7;
pub const TOKEN_SUCCESS: u32 = 8;
pub const TOKEN_SUCCESS_SOFT: u32 = 9;
pub const TOKEN_WARNING: u32 = 10;
pub const TOKEN_WARNING_SOFT: u32 = 11;
pub const TOKEN_SURFACE: u32 = 12;
pub const TOKEN_SURFACE_MUTED: u32 = 13;
pub const TOKEN_SURFACE_SUNKEN: u32 = 14;
pub const TOKEN_HOVER: u32 = 15;
pub const TOKEN_BORDER: u32 = 16;
pub const TOKEN_BORDER_STRONG: u32 = 17;
pub const TOKEN_TRANSPARENT: u32 = 18;

pub const WEIGHT_NORMAL: u32 = 0;
pub const WEIGHT_MEDIUM: u32 = 1;
pub const WEIGHT_SEMI_BOLD: u32 = 2;

pub const FAMILY_UI: u32 = 0;
pub const FAMILY_MONO: u32 = 1;

pub const ALIGN_START: u32 = 0;
pub const ALIGN_CENTER: u32 = 1;
pub const ALIGN_END: u32 = 2;

pub const IMAGE_PRIVATE: u32 = 1;
pub const IMAGE_VAULT: u32 = 2;
pub const IMAGE_URL: u32 = 3;
pub const IMAGE_FILE: u32 = 4;

pub const FIT_FILL: u32 = 0;
pub const FIT_CONTAIN: u32 = 1;
pub const FIT_COVER: u32 = 2;

pub const CURSOR_DEFAULT: u32 = 0;
pub const CURSOR_POINTER: u32 = 1;
pub const CURSOR_TEXT: u32 = 2;
pub const CURSOR_GRAB: u32 = 3;
pub const CURSOR_GRABBING: u32 = 4;
pub const CURSOR_CROSSHAIR: u32 = 5;
pub const CURSOR_NOT_ALLOWED: u32 = 6;
pub const CURSOR_RESIZE_H: u32 = 7;
pub const CURSOR_RESIZE_V: u32 = 8;

/// 面类型，取值与插件协议的 `SurfaceKind` 一一对应。
pub const SURFACE_PANEL: u32 = 1;
pub const SURFACE_WINDOW: u32 = 2;
pub const SURFACE_PAGE: u32 = 3;
pub const SURFACE_TAKEOVER: u32 = 4;
pub const SURFACE_SETTINGS: u32 = 5;
pub const SURFACE_TABLE_VIEW: u32 = 6;

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

pub const BUTTON_LEFT: u32 = 0;
pub const BUTTON_RIGHT: u32 = 1;
pub const BUTTON_MIDDLE: u32 = 2;

pub const MOD_SHIFT: u32 = 1;
pub const MOD_CONTROL: u32 = 2;
pub const MOD_ALT: u32 = 4;
pub const MOD_META: u32 = 8;

/// 键盘按键：`KEY_CHARACTER` 表示可打印字符（字符在 `AbiInput.text` 里），
/// 其余取值是具名编辑 / 功能键。
pub const KEY_CHARACTER: u32 = 0;
pub const KEY_ESCAPE: u32 = 1;
pub const KEY_ENTER: u32 = 2;
pub const KEY_TAB: u32 = 3;
pub const KEY_SPACE: u32 = 4;
pub const KEY_BACKSPACE: u32 = 5;
pub const KEY_DELETE: u32 = 6;
pub const KEY_INSERT: u32 = 7;
pub const KEY_HOME: u32 = 8;
pub const KEY_END: u32 = 9;
pub const KEY_PAGE_UP: u32 = 10;
pub const KEY_PAGE_DOWN: u32 = 11;
pub const KEY_ARROW_UP: u32 = 12;
pub const KEY_ARROW_DOWN: u32 = 13;
pub const KEY_ARROW_LEFT: u32 = 14;
pub const KEY_ARROW_RIGHT: u32 = 15;
pub const KEY_F1: u32 = 16;
pub const KEY_F2: u32 = 17;
pub const KEY_F3: u32 = 18;
pub const KEY_F4: u32 = 19;
pub const KEY_F5: u32 = 20;
pub const KEY_F6: u32 = 21;
pub const KEY_F7: u32 = 22;
pub const KEY_F8: u32 = 23;
pub const KEY_F9: u32 = 24;
pub const KEY_F10: u32 = 25;
pub const KEY_F11: u32 = 26;
pub const KEY_F12: u32 = 27;

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

    pub fn is_empty(&self) -> bool {
        self.data.is_null() || self.len == 0
    }
}

/// 颜色：RGBA 或主题 token。`token` 仅在 `kind == COLOR_TOKEN` 时有意义。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiColor {
    pub kind: u32,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
    pub token: u32,
}

impl AbiColor {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { kind: COLOR_RGBA, r, g, b, a, token: 0 }
    }

    pub const fn token(token: u32) -> Self {
        Self { kind: COLOR_TOKEN, r: 0, g: 0, b: 0, a: 0, token }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiPoint {
    pub x: f32,
    pub y: f32,
}

/// 矩形投影：只对矩形生效，绘制在填充与描边之下。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub color: AbiColor,
    pub enabled: u8,
}

impl AbiShadow {
    pub const NONE: Self =
        Self { offset_x: 0.0, offset_y: 0.0, blur: 0.0, color: AbiColor::TRANSPARENT, enabled: 0 };
}

/// 矩形与通用几何：填充 / 描边各自用 `has_*` 开关；命中区与输入法光标只用坐标。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub fill: AbiColor,
    pub stroke: AbiColor,
    pub stroke_width: f32,
    pub has_fill: u8,
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
            stroke: AbiColor::TRANSPARENT,
            stroke_width: 1.0,
            has_fill: 1,
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
            fill: AbiColor::TRANSPARENT,
            stroke: AbiColor::TRANSPARENT,
            stroke_width: 1.0,
            has_fill: 0,
            has_stroke: 0,
        }
    }
}

/// 一条绘制指令；按 `kind` 取用对应字段，其余字段保持零值。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiCommand {
    pub kind: u32,
    pub rect: AbiRect,
    pub shadow: AbiShadow,
    /// 线终点；线起点用 `rect.x / rect.y`。
    pub line_x2: f32,
    pub line_y2: f32,
    /// 圆：圆心与半径；填充 / 描边仍取 `rect` 的字段。
    pub circle_cx: f32,
    pub circle_cy: f32,
    pub circle_r: f32,
    /// 路径点；`closed != 0` 时闭合。
    pub points: *const AbiPoint,
    pub points_len: usize,
    pub closed: u8,
    /// 文字：定位用 `rect.x / rect.y`；`text_max_width <= 0` 表示不换行。
    pub text: AbiStr,
    pub text_size: f32,
    pub text_color: AbiColor,
    pub text_weight: u32,
    pub text_family: u32,
    pub text_align: u32,
    pub text_max_width: f32,
    /// 图片：用 `rect` 定位，来源按 `image_source` 解释。
    pub image_source: u32,
    pub image_path: AbiStr,
    /// 仅 URL 来源有意义：按本机 / 局域网策略取图。
    pub image_local: u8,
    pub image_fit: u32,
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
    pub has_ime: u8,
    pub ime: AbiRect,
    /// 非 0 = 本面承接键盘。
    pub keys: u8,
    /// 帧率（0 = 不推帧拍）。
    pub fps: u32,
}

/// 一条原始输入；坐标面本地，滚轮增量统一为像素。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AbiInput {
    pub kind: u32,
    pub x: f32,
    pub y: f32,
    /// 面尺寸（`INPUT_RESIZE`）。
    pub width: f32,
    pub height: f32,
    pub button: u32,
    /// 具名键编码或 `KEY_CHARACTER`。
    pub key: u32,
    /// 字符键的文本、输入法提交 / 组合串。
    pub text: AbiStr,
    /// 指针落在的命中区 id（未命中为空）。
    pub hit: AbiStr,
    /// 指针按下、按键按下、焦点获得。
    pub pressed: u8,
    pub modifiers: u32,
    pub delta_x: f32,
    pub delta_y: f32,
    /// 输入法组合串更新中。
    pub composing: u8,
    pub has_caret: u8,
    pub caret: u32,
    pub candidates: AbiRect,
    pub has_candidates: u8,
}

/// 宿主回执：宿主必须恰好调用一次，可从任意宿主线程调用；`payload` 只在调用内有效。
/// `None`（空指针）表示调用方无从回执，宿主只记日志不回调。
pub type HostReplyFn =
    unsafe extern "C" fn(ctx: *mut c_void, request_id: u64, ok: u8, payload: AbiStr);

/// 宿主提供给插件的函数表；插件只经它回调宿主，可从任意插件线程调用。
/// `ctx` 与函数指针在实例生命周期内有效（`create` 收到的即后续回调共用的同一份）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub abi_version: u32,
    pub ctx: *mut c_void,
    pub submit_frame: unsafe extern "C" fn(*mut c_void, AbiStr, *const AbiFrame),
    pub request_redraw: unsafe extern "C" fn(*mut c_void, AbiStr),
    pub log: unsafe extern "C" fn(*mut c_void, u32, AbiStr),
    /// 调用宿主能力：`method` 为完整方法名（如 `vault.readText`），`params` 为 JSON。
    pub call_host: unsafe extern "C" fn(
        *mut c_void,
        u64,
        AbiStr,
        AbiStr,
        Option<HostReplyFn>,
        *mut c_void,
    ),
    pub subscribe: unsafe extern "C" fn(*mut c_void, AbiStr),
    /// 回报一次宿主发起的服务调用；`call_id` 来自 `on_service_call`。
    pub reply_service: unsafe extern "C" fn(*mut c_void, u64, u8, AbiStr),
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
    /// 面挂载：`surface` + 面类型码；尺寸由随后的 `INPUT_RESIZE` 给出。
    pub on_attach: unsafe extern "C" fn(*mut c_void, AbiStr, u32),
    pub on_detach: unsafe extern "C" fn(*mut c_void, AbiStr),
    pub on_input: unsafe extern "C" fn(*mut c_void, AbiStr, *const AbiInput),
    /// 帧拍：`frame` 从 0 起，`delta_ms` 为距上一拍的毫秒数。
    pub on_tick: unsafe extern "C" fn(*mut c_void, AbiStr, u64, u32),
    /// 宿主通知：`name` 为 `host:` 方法名，`payload` 为 JSON。
    pub on_event: unsafe extern "C" fn(*mut c_void, AbiStr, AbiStr),
    /// 宿主调用插件提供的服务；插件用 `HostApi.reply_service` 回报。
    pub on_service_call:
        unsafe extern "C" fn(*mut c_void, u64, AbiStr, AbiStr, AbiStr),
}

pub type PluginApiEntry = unsafe extern "C" fn() -> *const PluginApi;
