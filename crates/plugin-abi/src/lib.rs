//! 同进程原生插件的 C ABI：宿主导出回调函数表 [`HostApi`]，插件导出创建 / 投递 / 销毁三个符号。
//! 两侧都以本 crate 的符号名与函数类型为准，字节流仍是协议信封的 NDJSON——与进程承载同一条通路，
//! 差别只在「管道」换成函数调用。

use std::ffi::{c_char, c_void, CStr};

/// ABI 版本：宿主与插件不一致即拒绝装载（不兼容就响亮失败，不做兼容层）。
pub const ABI_VERSION: u32 = 1;

/// 插件导出的三个符号名。宿主按名解析，导出宏按名声明——两处必须一致。
pub const CREATE_SYMBOL: &str = "atelyx_plugin_create";
pub const SEND_SYMBOL: &str = "atelyx_plugin_send";
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

/// 插件实例句柄：`create` 返回，`send` / `destroy` 首参。
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

/// 插件导出：按宿主函数表建立会话，返回句柄；失败返回空指针。
pub type CreateFn = unsafe extern "C" fn(api: *const HostApi) -> PluginHandle;
/// 插件导出：投递一条宿主下发的信封（UTF-8，不带换行），返回 [`SEND_OK`] / [`SEND_DROPPED`] /
/// [`SEND_CLOSED`]。
pub type SendFn = unsafe extern "C" fn(handle: PluginHandle, ptr: *const u8, len: usize) -> u32;
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
}
