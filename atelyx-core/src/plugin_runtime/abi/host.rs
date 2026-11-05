//! 同进程插件动态库的装载层：libloading 加载、契约版本严格相等校验、实例生命周期。
//! 跨动态库只走 `atelyx-plugin-abi` 的 C 布局值；帧在回调返回前复制成协议帧。

use std::ffi::{c_void, OsStr};
use std::sync::Arc;

use atelyx_plugin_abi as abi;
use atelyx_plugin_protocol::frame::SurfaceFrame;

use super::frame::{copy_str, frame_from_abi};

/// 宿主需要实现的全部回调；插件可在自己的线程上调用，因此实现必须 `Send + Sync`，且不得 panic。
pub trait Host: Send + Sync {
    /// 一帧已复制到宿主内存的协议帧；返回后插件可立即复用指令与字符串内存。
    fn submit_frame(&self, surface: &str, frame: SurfaceFrame);
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
    /// 加载动态库并校验导出的契约版本必须与宿主严格相等。
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
        check_contract(api.abi_version)?;
        Ok(Self { _lib: lib, api })
    }

    /// 创建一个插件实例；`host` 在实例销毁前必须保持有效。
    pub fn create(self: &Arc<Self>, host: Arc<dyn Host>) -> Result<PluginInstance, String> {
        let mut host = Box::new(HostCtx {
            host,
            api: abi::HostApi {
                abi_version: abi::CONTRACT_VERSION,
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
            return Err("插件 create 返回空句柄（契约版本不符或初始化失败）".into());
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

/// 契约版本必须严格相等；宽容匹配会让旧插件带未知语义跑起来。
fn check_contract(version: u32) -> Result<(), String> {
    if version == abi::CONTRACT_VERSION {
        Ok(())
    } else {
        Err(format!("契约版本不匹配：插件 {version}，宿主 {}", abi::CONTRACT_VERSION))
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
        match unsafe { frame_from_abi(&*frame) } {
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
    fn contract_version_must_match_exactly() {
        assert!(check_contract(abi::CONTRACT_VERSION).is_ok());
        assert!(check_contract(abi::CONTRACT_VERSION + 1).is_err());
    }
}
