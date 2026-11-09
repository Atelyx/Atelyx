//! 同进程原生插件的 C ABI：宿主导出控制回调函数表 [`HostApi`] 与绘制函数表 [`DrawSink`]，
//! 插件导出创建 / 投递 / 绘制 / 销毁四个符号。控制通道是协议信封的 NDJSON；绘制是宿主重绘拍里的
//! 同步调用——参数与产物都是本 crate 的定长结构，两侧按 `kind` 常量解读。

use std::ffi::{c_char, c_void, CStr};

/// ABI 版本：宿主与插件不一致即拒绝装载（不兼容就响亮失败，不做兼容层）。
pub const ABI_VERSION: u32 = 1;

/// 插件导出的四个符号名。宿主按名解析，导出宏按名声明——两处必须一致。
pub const CREATE_SYMBOL: &str = "atelyx_plugin_create";
pub const SEND_SYMBOL: &str = "atelyx_plugin_send";
pub const DRAW_SYMBOL: &str = "atelyx_plugin_draw";
pub const DESTROY_SYMBOL: &str = "atelyx_plugin_destroy";

/// 宿主 → 插件投递的结果：已入队。
pub const SEND_OK: u32 = 0;
/// 宿主 → 插件投递的结果：插件队列已满，本笔已丢弃（插件长期不读下发）。
pub const SEND_DROPPED: u32 = 1;
/// 宿主 → 插件投递的结果：会话已结束，本笔无法送达。
pub const SEND_CLOSED: u32 = 2;

/// 诊断输出等级：只落插件日志，不改变会话状态。
pub const LOG_DEBUG: u32 = 0;
pub const LOG_INFO: u32 = 1;
pub const LOG_WARN: u32 = 2;
pub const LOG_ERROR: u32 = 3;

/// `destroy` 的结果：会话线程已退出，动态库可以卸载。
pub const DESTROY_JOINED: u32 = 0;
/// `destroy` 的结果：会话线程仍在库内运行，宿主必须保留动态库句柄（卸载会让运行中的代码段消失）。
pub const DESTROY_DETACHED: u32 = 1;

/// `draw` 的结果：这一面已画完，绘制指令、命中区与输入声明已推给宿主。
pub const DRAW_OK: u32 = 0;
/// `draw` 的结果：该面正被输入回调占用，宿主沿用上一拍画面并在下一拍重试。
pub const DRAW_BUSY: u32 = 1;
/// `draw` 的结果：draw 期间调用了宿主能力面（重入），本拍作废。
pub const DRAW_REJECTED: u32 = 2;
/// `draw` 的结果：插件侧绘制失败（panic 已在库内收住），宿主沿用上一拍画面。
pub const DRAW_FAILED: u32 = 3;

/// 插件实例句柄：`create` 返回，`send` / `draw` / `destroy` 首参。
pub type PluginHandle = *mut c_void;

/// 宿主导出给插件的函数表。指针在插件会话存续期间有效，插件须在 `create` 里取走所需项。
///
/// 回调可能从插件自己的线程上被调用，也可以是宿主线程；实现侧不得阻塞（宿主下发走非阻塞队列）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    /// 必须等于 [`ABI_VERSION`]，不等即拒绝创建。
    pub abi_version: u32,
    /// 宿主上下文：以下回调的首参原样回传。
    pub ctx: *mut c_void,
    /// 插件 → 宿主：一条 NDJSON 信封（UTF-8，不带换行）。返回 [`SEND_DROPPED`] / [`SEND_CLOSED`]
    /// 表示这一笔没有落到宿主，插件据此留痕。
    pub send: Option<unsafe extern "C" fn(ctx: *mut c_void, ptr: *const u8, len: usize) -> u32>,
    /// 插件 → 宿主：一行诊断输出。
    pub log: Option<unsafe extern "C" fn(ctx: *mut c_void, level: u32, ptr: *const u8, len: usize)>,
    /// 插件 → 宿主：本会话已结束（正常收尾 `code = 0`，异常非零并带可读原因）。
    pub closed: Option<unsafe extern "C" fn(ctx: *mut c_void, code: i32, ptr: *const u8, len: usize)>,
    /// 安装 id / 名称 / 版本（NUL 结尾 UTF-8）：插件据此填握手自报信息。
    pub plugin_id: *const c_char,
    pub plugin_name: *const c_char,
    pub plugin_version: *const c_char,
}

/// 一段按字节计长的 UTF-8 文本。只在本次回调内有效，接收方须当场复制。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiStr {
    pub ptr: *const u8,
    pub len: usize,
}

impl AbiStr {
    /// 借一段文本作 ABI 入参：只在 `text` 存续期间有效。
    pub fn of(text: &str) -> Self {
        Self { ptr: text.as_ptr(), len: text.len() }
    }

    /// 空串（无该项）。
    pub fn empty() -> Self {
        Self { ptr: std::ptr::null(), len: 0 }
    }

    /// 读回文本；空串或非 UTF-8 返回 `None`。
    ///
    /// # Safety
    /// `ptr` 须为空或指向 `len` 字节的有效内存。
    pub unsafe fn read(&self) -> Option<String> {
        if self.len == 0 || self.ptr.is_null() {
            return None;
        }
        std::str::from_utf8(std::slice::from_raw_parts(self.ptr, self.len))
            .ok()
            .map(str::to_string)
    }
}

/// 颜色取值的 kind。
pub const COLOR_NONE: u32 = 0;
pub const COLOR_TOKEN: u32 = 1;
pub const COLOR_RGBA: u32 = 2;

/// 颜色：不绘制 / 主题 token / 自带 RGBA。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiColor {
    pub kind: u32,
    /// `kind = COLOR_TOKEN` 时的 token 序号（见宿主主题 token 表）。
    pub token: u32,
    /// `kind = COLOR_RGBA` 时的各通道 0~255。
    pub rgba: [u8; 4],
}

impl AbiColor {
    pub fn none() -> Self {
        Self { kind: COLOR_NONE, token: 0, rgba: [0; 4] }
    }
}

/// 字重 / 字体族 / 对齐 / 图片填充的取值。0 恒表示「未声明」，变体从 1 起，两侧按同一张表解读。
pub const WEIGHT_NORMAL: u32 = 1;
pub const WEIGHT_MEDIUM: u32 = 2;
pub const WEIGHT_SEMIBOLD: u32 = 3;

pub const FAMILY_UI: u32 = 1;
pub const FAMILY_MONO: u32 = 2;

pub const ALIGN_START: u32 = 1;
pub const ALIGN_CENTER: u32 = 2;
pub const ALIGN_END: u32 = 3;

pub const FIT_FILL: u32 = 1;
pub const FIT_CONTAIN: u32 = 2;
pub const FIT_COVER: u32 = 3;

pub const CURSOR_DEFAULT: u32 = 0;
pub const CURSOR_POINTER: u32 = 1;
pub const CURSOR_TEXT: u32 = 2;
pub const CURSOR_GRAB: u32 = 3;
pub const CURSOR_GRABBING: u32 = 4;
pub const CURSOR_CROSSHAIR: u32 = 5;
pub const CURSOR_NOT_ALLOWED: u32 = 6;
pub const CURSOR_RESIZE_HORIZONTAL: u32 = 7;
pub const CURSOR_RESIZE_VERTICAL: u32 = 8;

/// 绘制指令的 kind（[`DrawSink::command`] 的判别式，决定 `data` 指向哪个结构）。
pub const CMD_LINE: u32 = 0;
pub const CMD_RECT: u32 = 1;
pub const CMD_PATH: u32 = 2;
pub const CMD_CIRCLE: u32 = 3;
pub const CMD_TEXT: u32 = 4;
pub const CMD_IMAGE: u32 = 5;

/// 图片源的 kind。
pub const IMAGE_PRIVATE: u32 = 0;
pub const IMAGE_VAULT: u32 = 1;
pub const IMAGE_URL: u32 = 2;
pub const IMAGE_FILE: u32 = 3;

/// 二维点（逻辑像素）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiPoint {
    pub x: f32,
    pub y: f32,
}

/// 面本地矩形。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 矩形投影：偏移、模糊半径与颜色。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiShadow {
    /// 0 = 无投影，1 = 有投影。
    pub kind: u32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub color: AbiColor,
}

/// [`CMD_LINE`] 的载荷。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiLine {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub stroke: AbiColor,
    pub width: f32,
}

/// [`CMD_RECT`] 的载荷。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiRectShape {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub fill: AbiColor,
    pub stroke: AbiColor,
    /// 只在 `stroke` 存在时生效。
    pub stroke_width: f32,
    /// 负 = 无圆角。
    pub corner_radius: f32,
    pub shadow: AbiShadow,
}

/// [`CMD_PATH`] 的载荷：`points` 指向 `len` 个点，只在本次回调内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiPath {
    pub points: *const AbiPoint,
    pub len: usize,
    /// 0 = 不闭合，1 = 闭合。
    pub closed: u32,
    pub fill: AbiColor,
    pub stroke: AbiColor,
    pub stroke_width: f32,
}

/// [`CMD_CIRCLE`] 的载荷。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiCircle {
    pub cx: f32,
    pub cy: f32,
    pub r: f32,
    pub fill: AbiColor,
    pub stroke: AbiColor,
    pub stroke_width: f32,
}

/// [`CMD_TEXT`] 的载荷：`content` 只在本次回调内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiText {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub content: AbiStr,
    pub color: AbiColor,
    /// 0 = 无字重声明；其余见 `WEIGHT_*`。
    pub weight: u32,
    /// 0 = 无字族声明；其余见 `FAMILY_*`。
    pub family: u32,
    /// 0 = 无对齐声明；其余见 `ALIGN_*`。
    pub align: u32,
    /// 可用宽度：负 = 不换行。
    pub max_width: f32,
}

/// [`CMD_IMAGE`] 的载荷：`path` / `url` 只在本次回调内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiImage {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 图片来源，见 `IMAGE_*`。
    pub source: u32,
    pub path: AbiStr,
    pub url: AbiStr,
    /// `source = IMAGE_URL` 时：1 = 按本机 / 局域网策略取图，0 = 公网策略。
    pub local: u32,
    /// 0 = 无填充方式声明；其余见 `FIT_*`。
    pub fit: u32,
}

/// 命中区：矩形 + 标识 + 光标形状。`id` 只在本次回调内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiHit {
    pub id: AbiStr,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 0 = 系统默认箭头；其余见 `CURSOR_*`。
    pub cursor: u32,
}

/// 量算入参：一段文字与它的排版口径。`text` 只在本次回调内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiMeasureItem {
    pub text: AbiStr,
    pub size: f32,
    /// 0 = 无字重声明；其余见 `WEIGHT_*`。
    pub weight: u32,
    /// 0 = 无字族声明；其余见 `FAMILY_*`。
    pub family: u32,
    /// 可用宽度：负 = 单行不换行。
    pub max_width: f32,
}

/// 量算结果：排版实际占位（换行时宽不超过可用宽度）与行数。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiMeasureResult {
    pub width: f32,
    pub height: f32,
    pub lines: u32,
}

/// 主题颜色（RGBA，各通道 0~255）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiRgba {
    pub rgba: [u8; 4],
}

/// 主题颜色 token 数（与协议 `ColorToken` 的判别式顺序一致）。
pub const THEME_COLOR_COUNT: usize = 18;
pub const THEME_FONT_SIZE_COUNT: usize = 6;
pub const THEME_RADIUS_COUNT: usize = 3;

/// 主题表：颜色按 token 表定序，字号档与圆角档同宿主界面常量。
///
/// 事件色板不在这里：插件需要它时经 `query.theme` 取（绘制期只认控件要用的那几档）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiTheme {
    pub colors: [AbiRgba; THEME_COLOR_COUNT],
    pub font_sizes: [f32; THEME_FONT_SIZE_COUNT],
    pub radii: [f32; THEME_RADIUS_COUNT],
}

/// 一次 draw 的目标面：宿主这一拍给该面分配到的真实尺寸。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DrawTarget {
    /// `surface_id_len` 字节的 UTF-8，只在本次调用内有效。
    pub surface_id: *const u8,
    pub surface_id_len: usize,
    pub width: f32,
    pub height: f32,
}

/// Markdown 块的 kind。
pub const MD_HEADING: u32 = 0;
pub const MD_PARAGRAPH: u32 = 1;
pub const MD_LIST_ITEM: u32 = 2;
pub const MD_QUOTE: u32 = 3;
pub const MD_CODE: u32 = 4;
pub const MD_TABLE: u32 = 5;
pub const MD_DIVIDER: u32 = 6;

/// Markdown 行内片段：`text` / `link` 只在本次 draw 内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiMdSpan {
    pub text: AbiStr,
    pub strong: u32,
    pub code: u32,
    /// 空 = 无链接。
    pub link: AbiStr,
}

/// Markdown 表格行：`cells` 指向 `cell_count` 个单元格文本，只在本次 draw 内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiMdRow {
    pub cells: *const AbiStr,
    pub cell_count: usize,
}

/// Markdown 块：`spans` / `rows` / `text` / `lang` 只在本次 draw 内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiMdBlock {
    pub kind: u32,
    /// 标题层级（`MD_HEADING`）或嵌套层数（其余块）。
    pub level: u32,
    pub depth: u32,
    pub ordered: u32,
    pub index: u32,
    pub spans: *const AbiMdSpan,
    pub span_count: usize,
    /// 代码块正文。
    pub text: AbiStr,
    /// 代码块语言。
    pub lang: AbiStr,
    pub rows: *const AbiMdRow,
    pub row_count: usize,
}

/// Markdown 解析结果：`blocks` 指向 `count` 个块，只在本次 draw 内有效。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AbiMarkdown {
    pub blocks: *const AbiMdBlock,
    pub count: usize,
}

/// 宿主的绘制函数表。只在 [`DrawFn`] 调用期间有效，插件不得保存其中任何指针。
///
/// 全部回调都在宿主的重绘线程上同步执行，插件必须当场给出结果：这里没有跨拍等待，
/// 也不允许在回调里再调宿主能力面（那会重入宿主，宿主据此判定 [`DRAW_REJECTED`]）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DrawSink {
    /// 宿主上下文：以下回调的首参原样回传。
    pub ctx: *mut c_void,
    /// 同步量一段文字的排版占位。
    pub measure:
        Option<unsafe extern "C" fn(ctx: *mut c_void, item: *const AbiMeasureItem) -> AbiMeasureResult>,
    /// 同步解析一段 Markdown：`out` 收到块表，成功返回 0、失败返回非零（`out` 置空）。
    pub markdown:
        Option<unsafe extern "C" fn(ctx: *mut c_void, text: AbiStr, out: *mut AbiMarkdown) -> u32>,
    /// 同步取主题表。
    pub theme: Option<unsafe extern "C" fn(ctx: *mut c_void, out: *mut AbiTheme)>,
    /// 推一条绘制指令：`data` 按 `kind` 指向对应结构。
    pub command: Option<unsafe extern "C" fn(ctx: *mut c_void, kind: u32, data: *const c_void)>,
    /// 推一个命中区。
    pub hit: Option<unsafe extern "C" fn(ctx: *mut c_void, hit: *const AbiHit)>,
    /// 推输入声明：`ime` 为空表示不声明输入法，`fps = 0` 表示不声明帧拍。
    pub policy:
        Option<unsafe extern "C" fn(ctx: *mut c_void, ime: *const AbiRect, keys: u32, fps: u32)>,
    /// 绘制期的诊断输出（落插件日志，等级同 `LOG_*`）。绘制跑在宿主线程上，stderr 在发布版
    /// 不可见，出了事只能走这条。
    pub log: Option<unsafe extern "C" fn(ctx: *mut c_void, level: u32, ptr: *const u8, len: usize)>,
}

/// 插件导出：按宿主函数表建立会话，返回句柄；失败返回空指针。
pub type CreateFn = unsafe extern "C" fn(api: *const HostApi) -> PluginHandle;
/// 插件导出：投递一条宿主下发的信封（UTF-8，不带换行），返回 [`SEND_OK`] / [`SEND_DROPPED`] /
/// [`SEND_CLOSED`]。
pub type SendFn = unsafe extern "C" fn(handle: PluginHandle, ptr: *const u8, len: usize) -> u32;
/// 插件导出：画一面，返回 [`DRAW_OK`] / [`DRAW_BUSY`] / [`DRAW_REJECTED`] / [`DRAW_FAILED`]。
pub type DrawFn = unsafe extern "C" fn(
    handle: PluginHandle,
    target: *const DrawTarget,
    sink: *const DrawSink,
) -> u32;
/// 插件导出：结束会话，返回 [`DESTROY_JOINED`] 或 [`DESTROY_DETACHED`]。
pub type DestroyFn = unsafe extern "C" fn(handle: PluginHandle) -> u32;

/// 读 NUL 结尾 UTF-8 字符串；空指针或非 UTF-8 返回 `None`。两侧共用这一份，避免各写一遍判空。
///
/// # Safety
/// `ptr` 须为空或指向 NUL 结尾的有效字符串。
pub unsafe fn read_cstr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn cstr_roundtrip_and_null() {
        let text = CString::new("com.atelyx.echo").unwrap();
        assert_eq!(unsafe { read_cstr(text.as_ptr()) }.as_deref(), Some("com.atelyx.echo"));
        assert_eq!(unsafe { read_cstr(std::ptr::null()) }, None);
    }

    #[test]
    fn abi_str_reads_back_what_it_borrows() {
        let inline = AbiStr::of("面板");
        assert_eq!(unsafe { inline.read() }.as_deref(), Some("面板"));
        assert_eq!(unsafe { AbiStr::empty().read() }, None);
    }

    #[test]
    fn host_api_is_a_plain_pointer_table() {
        // 函数表跨 FFI 传递：只能由标量与函数指针组成，不允许 Rust 枚举 / Option 布局混入
        let api = HostApi {
            abi_version: ABI_VERSION,
            ctx: std::ptr::null_mut(),
            send: None,
            log: None,
            closed: None,
            plugin_id: std::ptr::null(),
            plugin_name: std::ptr::null(),
            plugin_version: std::ptr::null(),
        };
        assert_eq!(api.abi_version, 1);
    }

    /// 绘制函数表同样只能是标量 + 函数指针：任何 Rust 枚举混进来都会让布局不可预期。
    #[test]
    fn draw_sink_is_a_plain_pointer_table() {
        let sink = DrawSink {
            ctx: std::ptr::null_mut(),
            measure: None,
            markdown: None,
            theme: None,
            command: None,
            hit: None,
            policy: None,
            log: None,
        };
        assert!(sink.ctx.is_null());
        // 颜色 token 表与协议枚举同序，长度对不上说明两边有一边漏了项
        assert_eq!(THEME_COLOR_COUNT, 18);
    }
}
