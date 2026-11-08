//! 同进程承载：装载插件动态库，经 C ABI 函数表双向投递 NDJSON 信封。
//! 与进程承载共用信封、握手、能力面与整面帧——差别只在「管道」换成函数调用，协议语义不变。
//!
//! 装载分两段：open 备好库与函数表，activate 才调插件的 create。会话线程一建立就发 initialize，
//! 宿主对它的应答可能早于 create 返回——这段窗口里的下发先排队，激活后按序补发，否则插件会等满
//! 握手超时。

use std::ffi::{c_void, CString};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use atelyx_plugin_abi as abi;
use atelyx_plugin_protocol::manifest::Manifest;
use libloading::Library;

use super::instance::Instance;

/// 会话阶段：创建前 / 已建立 / 已收尾。
enum Stage {
    /// 插件实例尚未创建：下发的应答先排队，激活后按序补发。
    Starting(Vec<String>),
    Ready(abi::PluginHandle),
    /// 已收尾（或创建被拒）：后续下发一律如实失败。
    Closed,
}

/// 一个同进程插件的会话：库句柄、导出符号、宿主函数表与回调落点。
pub(super) struct Loaded {
    /// `None` = 会话线程未能收尾，库句柄已交出（不再卸载）。
    library: Mutex<Option<Library>>,
    stage: Mutex<Stage>,
    create: abi::CreateFn,
    send: abi::SendFn,
    destroy: abi::DestroyFn,
    /// 宿主函数表：插件侧持有它的地址，会话期间不得移动；会话没收尾时连带常留（见 Drop）。
    host: Option<Box<abi::HostApi>>,
    /// 握手自报的三段字符串：函数表持有它们的数据指针，须与本结构同寿命。
    info: Option<SessionInfo>,
    /// 回调落点：函数表的 ctx 指向它的堆地址，由本结构持有强引用。
    sink: Arc<Sink>,
}

/// 插件自报信息（NUL 结尾 UTF-8）：指针在 CString 存活期间稳定，移动本结构不影响。
struct SessionInfo {
    id: CString,
    name: CString,
    version: CString,
}

/// 插件回调落点：三条回调都只做「投递到实例」，不做任何协议判断。
struct Sink {
    instance: Mutex<Weak<Instance>>,
    /// 建立会话时的运行令牌：回调只影响这一次会话。
    token: u64,
}

impl Sink {
    fn instance(&self) -> Option<Arc<Instance>> {
        let instance = self.instance.lock().unwrap().upgrade()?;
        (instance.run_token() == self.token).then_some(instance)
    }
}

// 会话可跨线程共享：函数表与自报字符串由本结构独占（Box / CString 的指针对内稳定），
// 动态库句柄只是装载凭据；插件侧自己保证会话线程的安全。
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

unsafe extern "C" fn host_send(ctx: *mut c_void, ptr: *const u8, len: usize) -> u32 {
    if ctx.is_null() || (ptr.is_null() && len > 0) {
        return abi::SEND_DROPPED;
    }
    let sink = &*(ctx as *const Sink);
    let Some(instance) = sink.instance() else {
        return abi::SEND_CLOSED;
    };
    let bytes = std::slice::from_raw_parts(ptr, len);
    // 非 UTF-8 与「stdout 被污染」同一条判定：按违规停机并给出可读原因，不静默丢
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            instance.handle_carrier_output(text);
            abi::SEND_OK
        }
        Err(e) => {
            instance.note_carrier_garbled(&e.to_string());
            abi::SEND_DROPPED
        }
    }
}

unsafe extern "C" fn host_log(ctx: *mut c_void, level: u32, ptr: *const u8, len: usize) {
    if ctx.is_null() || (ptr.is_null() && len > 0) {
        return;
    }
    let sink = &*(ctx as *const Sink);
    let Some(instance) = sink.instance() else {
        return;
    };
    let bytes = std::slice::from_raw_parts(ptr, len);
    instance.note_carrier_log(level, &String::from_utf8_lossy(bytes));
}

unsafe extern "C" fn host_closed(ctx: *mut c_void, code: i32, ptr: *const u8, len: usize) {
    if ctx.is_null() {
        return;
    }
    let sink = &*(ctx as *const Sink);
    let reason = if ptr.is_null() || len == 0 {
        String::new()
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned()
    };
    if let Some(instance) = sink.instance() {
        instance.handle_carrier_closed(code, &reason);
    }
}

impl Loaded {
    /// 备好动态库、导出符号、函数表与回调落点；插件实例尚未创建（见 activate）。
    pub(super) fn open(
        path: &Path,
        instance: &Arc<Instance>,
        manifest: &Manifest,
    ) -> Result<Self, String> {
        let library = unsafe { Library::new(path) }
            .map_err(|e| format!("装载动态库失败（{}）：{e}", path.display()))?;
        let create = symbol::<abi::CreateFn>(&library, path, abi::CREATE_SYMBOL)?;
        let send = symbol::<abi::SendFn>(&library, path, abi::SEND_SYMBOL)?;
        let destroy = symbol::<abi::DestroyFn>(&library, path, abi::DESTROY_SYMBOL)?;
        let info = SessionInfo {
            id: cstring(&manifest.id)?,
            name: cstring(&manifest.name)?,
            version: cstring(&manifest.version)?,
        };
        let sink = Arc::new(Sink {
            instance: Mutex::new(Arc::downgrade(instance)),
            token: instance.run_token(),
        });
        let host = Box::new(abi::HostApi {
            abi_version: abi::ABI_VERSION,
            ctx: Arc::as_ptr(&sink) as *mut c_void,
            send: Some(host_send),
            log: Some(host_log),
            closed: Some(host_closed),
            plugin_id: info.id.as_ptr(),
            plugin_name: info.name.as_ptr(),
            plugin_version: info.version.as_ptr(),
        });
        Ok(Self {
            library: Mutex::new(Some(library)),
            stage: Mutex::new(Stage::Starting(Vec::new())),
            create,
            send,
            destroy,
            host: Some(host),
            info: Some(info),
            sink,
        })
    }

    /// 创建插件实例并放行此前排队的下发。创建被拒返回可读原因（会话随即判为已收尾）。
    pub(super) fn activate(&self) -> Result<(), String> {
        let mut stage = self.stage.lock().unwrap();
        let Some(host) = self.host.as_ref() else {
            return Err("插件会话已收尾".into());
        };
        let handle = unsafe { (self.create)(host.as_ref() as *const abi::HostApi) };
        if handle.is_null() {
            *stage = Stage::Closed;
            return Err("插件未完成创建：函数表版本不符或初始化失败".into());
        }
        let queued = match std::mem::replace(&mut *stage, Stage::Ready(handle)) {
            Stage::Starting(queued) => queued,
            _ => Vec::new(),
        };
        // 排队期间产生的下发（典型是 initialize 的应答）按原序补发
        for line in queued {
            if let Err(e) = self.send_line(handle, &line) {
                *stage = Stage::Closed;
                return Err(e);
            }
        }
        Ok(())
    }

    /// 宿主 → 插件投递一行信封（含行尾换行）。创建窗口内先排队，收尾后如实失败。
    pub(super) fn deliver(&self, line: &str) -> Result<(), String> {
        let mut stage = self.stage.lock().unwrap();
        match &mut *stage {
            Stage::Starting(queued) => {
                queued.push(line.to_string());
                Ok(())
            }
            Stage::Ready(handle) => self.send_line(*handle, line),
            Stage::Closed => Err("插件会话已结束，下发未送达".into()),
        }
    }

    fn send_line(&self, handle: abi::PluginHandle, line: &str) -> Result<(), String> {
        match unsafe { (self.send)(handle, line.as_ptr(), line.len()) } {
            abi::SEND_OK => Ok(()),
            abi::SEND_DROPPED => Err("插件未接收本笔下发（队列已满或内容非法）".into()),
            _ => Err("插件会话已结束，下发未送达".into()),
        }
    }

    /// 结束会话；返回 true = 会话线程已退出、库可卸载。
    pub(super) fn shutdown(&self) -> bool {
        // 先摘投递阶段再调 destroy：插件侧的销毁等会话线程退出，那个线程可能正卡在一次投递上
        let previous = std::mem::replace(&mut *self.stage.lock().unwrap(), Stage::Closed);
        let Stage::Ready(handle) = previous else {
            return true;
        };
        let code = unsafe { (self.destroy)(handle) };
        code == abi::DESTROY_JOINED
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        if matches!(self.stage.get_mut().unwrap(), Stage::Closed) {
            return;
        }
        // 会话线程仍在库内跑：库、函数表与回调上下文一并常留——卸载会让运行中的代码段消失，
        // 释放函数表会让插件手上的指针悬空。代价是这一份库与上下文留到进程退出。
        if let Some(library) = self.library.get_mut().unwrap().take() {
            std::mem::forget(library);
        }
        if let Some(host) = self.host.take() {
            std::mem::forget(host);
        }
        if let Some(info) = self.info.take() {
            std::mem::forget(info);
        }
        std::mem::forget(self.sink.clone());
    }
}

/// 读一个导出符号；缺失即给出可读原因（含库路径与符号名）。
fn symbol<T: Copy>(library: &Library, path: &Path, name: &str) -> Result<T, String> {
    unsafe { library.get(name.as_bytes()).map(|symbol| *symbol) }
        .map_err(|e| format!("动态库缺少符号 {name}（{}）：{e}", path.display()))
}

fn cstring(text: &str) -> Result<CString, String> {
    CString::new(text).map_err(|_| format!("插件自报信息含 NUL：{text}"))
}
