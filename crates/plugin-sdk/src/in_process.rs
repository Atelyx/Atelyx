//! 插件承载：宿主经 C ABI 投递 NDJSON 信封（控制通道），并在自己的重绘拍里同步调绘制入口。
//! `export_in_process_plugin!` 导出 create / send / draw / destroy 四个符号；会话跑在插件自己的
//! tokio 运行时上（宿主线程只入队），绘制是唯一例外——它在宿主重绘线程上同步执行。

use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::pin::Pin;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc as tokio_mpsc;

use crate::abi;
use crate::connection::Connection;
use crate::context::DrawGuard;
use crate::frame_ui::{DrawFrame, FrameUi};
use crate::handler::PluginHandler;
use crate::protocol::handshake::PluginInfo;
use crate::surface::AbiSurface;

/// 会话线程自行收尾的等待上限：超时即视为仍在库内运行，动态库不能卸载。
const JOIN_TIMEOUT: Duration = Duration::from_secs(3);

/// 一个插件会话。句柄即本结构的裸指针，生命周期由宿主的 create / destroy 配对决定。
struct Session {
    /// 宿主 → 插件的投递口；摘掉即让插件读端读到 EOF。
    incoming: Mutex<Option<tokio_mpsc::UnboundedSender<Vec<u8>>>>,
    /// 会话线程结束信号。
    done: Receiver<()>,
    /// 会话线程 id：宿主在插件线程上调用 destroy 时不能自己等自己。
    thread_id: Mutex<Option<ThreadId>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// 处理器与各面控件层：绘制入口与输入回调共用（处理器按 `&self` 调）。
    handler: Arc<dyn PluginHandler>,
    surfaces: Surfaces,
}

/// 各面的控件层状态。
///
/// 输入回调在插件线程上等得到它；绘制在宿主重绘线程上只做非阻塞获取——拿不到（该面正被
/// 输入回调占用）就跳过这一拍，绝不让宿主重绘线程等插件。
#[derive(Clone, Default)]
pub(crate) struct Surfaces {
    slots: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<FrameUi>>>>>,
}

impl Surfaces {
    fn slot(&self, surface_id: &str) -> Arc<tokio::sync::Mutex<FrameUi>> {
        let mut slots = self.slots.lock().unwrap();
        slots.entry(surface_id.to_string()).or_default().clone()
    }

    /// 输入回调：等该面的控件层空出来（插件线程上等待可接受）。
    pub(crate) async fn lock(&self, surface_id: &str) -> tokio::sync::OwnedMutexGuard<FrameUi> {
        self.slot(surface_id).lock_owned().await
    }

    /// 绘制：拿不到就返回 `None`，由宿主沿用上一拍。
    pub(crate) fn try_lock(
        &self,
        surface_id: &str,
    ) -> Option<tokio::sync::OwnedMutexGuard<FrameUi>> {
        self.slot(surface_id).try_lock_owned().ok()
    }
}

/// 宿主导出的函数表在插件侧的副本（回调指针在会话期间恒定）。
#[derive(Clone, Copy)]
struct HostLink {
    send: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> u32,
    log: Option<unsafe extern "C" fn(*mut c_void, u32, *const u8, usize)>,
    closed: Option<unsafe extern "C" fn(*mut c_void, i32, *const u8, usize)>,
    ctx: *mut c_void,
}

// 函数表与宿主上下文由宿主保证在会话期间有效，插件可跨线程使用。
unsafe impl Send for HostLink {}
unsafe impl Sync for HostLink {}

impl HostLink {
    fn line(&self, text: &str) -> u32 {
        unsafe { (self.send)(self.ctx, text.as_ptr(), text.len()) }
    }

    fn log(&self, level: u32, text: &str) {
        if let Some(log) = self.log {
            unsafe { log(self.ctx, level, text.as_ptr(), text.len()) }
        }
    }

    fn closed(&self, code: i32, reason: &str) {
        if let Some(closed) = self.closed {
            unsafe { closed(self.ctx, code, reason.as_ptr(), reason.len()) }
        }
    }
}

/// 读端：宿主下发的字节按到达序进队，通道关闭等价于管道读到结尾。
struct Incoming {
    rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl AsyncRead for Incoming {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.pos >= self.buf.len() {
            self.buf.clear();
            self.pos = 0;
            match Pin::new(&mut self.rx).poll_recv(cx) {
                Poll::Ready(Some(chunk)) => self.buf = chunk,
                // 通道关闭 = 宿主结束会话：读端到此为止，连接据此正常收场
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let take = out.remaining().min(self.buf.len() - self.pos);
        out.put_slice(&self.buf[self.pos..self.pos + take]);
        self.pos += take;
        Poll::Ready(Ok(()))
    }
}

/// 写端：入队即返回，真正的宿主调用在投递任务上执行（插件不因宿主慢而卡住）。
struct Outgoing {
    tx: tokio_mpsc::UnboundedSender<Vec<u8>>,
}

impl AsyncWrite for Outgoing {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.tx.send(buf.to_vec()).is_err() {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "宿主已关闭连接",
            )));
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// 建立一次会话并返回句柄；函数表版本不符或缺 `send` 时返回空指针。
///
/// # Safety
/// `api` 须为空或指向宿主在本会话期间保持有效的 `HostApi`。
pub unsafe fn serve<H: PluginHandler>(
    api: *const crate::abi::HostApi,
    handler: H,
) -> crate::abi::PluginHandle {
    if api.is_null() {
        return std::ptr::null_mut();
    }
    let api = &*api;
    if api.abi_version != crate::abi::ABI_VERSION {
        return std::ptr::null_mut();
    }
    let Some(send) = api.send else {
        return std::ptr::null_mut();
    };
    let link = HostLink { send, log: api.log, closed: api.closed, ctx: api.ctx };
    let plugin = PluginInfo {
        id: cstr(api.plugin_id).unwrap_or_default(),
        name: cstr(api.plugin_name).unwrap_or_default(),
        version: cstr(api.plugin_version).unwrap_or_default(),
    };

    let (in_tx, in_rx) = tokio_mpsc::unbounded_channel::<Vec<u8>>();
    let (out_tx, out_rx) = tokio_mpsc::unbounded_channel::<Vec<u8>>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let session_thread = Arc::new(Mutex::new(None::<ThreadId>));
    let thread_marker = session_thread.clone();
    let handler: Arc<dyn PluginHandler> = Arc::new(handler);
    let session_handler = handler.clone();
    let surfaces = Surfaces::default();
    let session_surfaces = surfaces.clone();

    let thread = thread::Builder::new()
        .name(format!("atelyx-plugin-{}", plugin.id))
        .spawn(move || {
            *thread_marker.lock().unwrap() = Some(thread::current().id());
            // panic 在插件侧就收住：插件与宿主同地址空间，逃出去的 panic 会把整个应用带走
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_session(link, plugin, session_handler, session_surfaces, in_rx, out_tx, out_rx)
            }))
            .unwrap_or_else(|payload| Err(format!("插件会话 panic：{}", panic_message(&payload))));
            let (code, reason) = match outcome {
                Ok(()) => (0, String::new()),
                Err(reason) => {
                    // 会话失败先落诊断：宿主据此在插件日志里看到原因，而不只看到「非零结束」
                    link.log(crate::abi::LOG_ERROR, &reason);
                    (1, reason)
                }
            };
            link.closed(code, &reason);
            let _ = done_tx.send(());
        });
    let thread = match thread {
        Ok(thread) => thread,
        Err(e) => {
            link.closed(1, &format!("建立插件会话线程失败：{e}"));
            return std::ptr::null_mut();
        }
    };

    let session = Box::new(Session {
        incoming: Mutex::new(Some(in_tx)),
        done: done_rx,
        thread_id: Mutex::new(*session_thread.lock().unwrap()),
        thread: Mutex::new(Some(thread)),
        handler,
        surfaces,
    });
    Box::into_raw(session) as crate::abi::PluginHandle
}

/// 宿主下发一条信封（UTF-8，不带换行），返回 `SEND_OK` / `SEND_CLOSED`。
///
/// # Safety
/// `handle` 须是 `serve` 返回且尚未 `shutdown` 的句柄。
pub unsafe fn deliver(handle: crate::abi::PluginHandle, ptr: *const u8, len: usize) -> u32 {
    if handle.is_null() || ptr.is_null() {
        return crate::abi::SEND_CLOSED;
    }
    let session = &*(handle as *const Session);
    let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
    let incoming = session.incoming.lock().unwrap();
    let Some(tx) = incoming.as_ref() else {
        return crate::abi::SEND_CLOSED;
    };
    match tx.send(bytes) {
        // 队列无界：入队即算送达；插件长期不读由宿主侧的投递限流兜住
        Ok(()) => crate::abi::SEND_OK,
        Err(_) => crate::abi::SEND_CLOSED,
    }
}

/// 结束会话并归还句柄：返回 `DESTROY_JOINED` 表示会话线程已退出、动态库可卸载。
///
/// # Safety
/// `handle` 须是 `serve` 返回且未调用过本函数的句柄。
pub unsafe fn shutdown(handle: crate::abi::PluginHandle) -> u32 {
    if handle.is_null() {
        return crate::abi::DESTROY_JOINED;
    }
    let session = Box::from_raw(handle as *mut Session);
    // 摘掉投递口：插件读端读到 EOF，会话按正常收尾退出
    session.incoming.lock().unwrap().take();
    let on_session_thread = *session.thread_id.lock().unwrap() == Some(thread::current().id());
    // 宿主在插件线程上发起收尾（例如协议违规停机）时不能自己等自己
    let joined = !on_session_thread && matches!(session.done.recv_timeout(JOIN_TIMEOUT), Ok(()));
    if !joined {
        // 会话线程仍在库内运行：宿主据此保留动态库句柄，卸载会让运行中的代码段消失
        let thread = session.thread.lock().unwrap().take();
        drop(thread);
        std::mem::forget(session);
        return crate::abi::DESTROY_DETACHED;
    }
    let thread = session.thread.lock().unwrap().take();
    drop(session);
    if let Some(thread) = thread {
        let _ = thread.join();
    }
    crate::abi::DESTROY_JOINED
}


/// 宿主重绘拍里同步画一面：取该面的控件层状态、调插件的 `on_draw`，结果由宿主当场收下。
///
/// 返回值：`DRAW_OK` 画成；`DRAW_BUSY` 该面正被输入回调占用（宿主沿用上一拍）；
/// `DRAW_REJECTED` draw 里调了宿主能力面；`DRAW_FAILED` 插件 panic 或入参不可读。
///
/// # Safety
/// `handle` 须是 `serve` 返回且尚未 `shutdown` 的句柄；`target` / `sink` 由宿主在调用期间持有。
pub unsafe fn draw_surface(
    handle: crate::abi::PluginHandle,
    target: *const abi::DrawTarget,
    sink: *const abi::DrawSink,
) -> u32 {
    if handle.is_null() || target.is_null() || sink.is_null() {
        return abi::DRAW_FAILED;
    }
    let session = &*(handle as *const Session);
    let target = &*target;
    let sink = &*sink;
    let id = std::slice::from_raw_parts(target.surface_id, target.surface_id_len);
    let Ok(surface_id) = std::str::from_utf8(id) else {
        return abi::DRAW_FAILED;
    };
    let Some(mut ui) = session.surfaces.try_lock(surface_id) else {
        return abi::DRAW_BUSY;
    };
    let surface = AbiSurface::new(sink, surface_id, target.width, target.height);
    let guard = DrawGuard::enter();
    // panic 不能越过 FFI 边界：收住后按绘制失败上报，宿主沿用上一拍
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut frame = DrawFrame::new(&surface, &mut ui);
        session.handler.on_draw(&mut frame);
        frame.failed()
    }));
    match outcome {
        Ok(false) => {}
        Ok(true) => {
            // 控件层校验失败：插件可能忽略了 build 的错误，这里替它留痕
            log_draw(sink, &format!("面 {surface_id} 绘制失败：帧不合法"));
            return abi::DRAW_FAILED;
        }
        Err(payload) => {
            log_draw(sink, &format!("面 {surface_id} 绘制 panic：{}", panic_message(&payload)));
            return abi::DRAW_FAILED;
        }
    }
    if guard.violated() {
        return abi::DRAW_REJECTED;
    }
    abi::DRAW_OK
}

/// 绘制期的一条诊断：走宿主的绘制函数表落插件日志（发布版没有 stderr 可看）。
fn log_draw(sink: &abi::DrawSink, text: &str) {
    if let Some(log) = sink.log {
        unsafe { log(sink.ctx, abi::LOG_ERROR, text.as_ptr(), text.len()) };
    }
}

/// 会话主体：跑在本插件专用的 tokio 运行时上，握手与全部交互交给 `Connection`。
fn run_session(
    link: HostLink,
    plugin: PluginInfo,
    handler: Arc<dyn PluginHandler>,
    surfaces: Surfaces,
    in_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    out_tx: tokio_mpsc::UnboundedSender<Vec<u8>>,
    mut out_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name(format!("atelyx-plugin-{}", plugin.id))
        .build()
        .map_err(|e| format!("建立插件运行时失败：{e}"))?;
    let result = runtime.block_on(async move {
        let drain = tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                for line in text.lines() {
                    if !line.is_empty() {
                        link.line(line);
                    }
                }
            }
        });
        let input = Incoming { rx: in_rx, buf: Vec::new(), pos: 0 };
        let output = Outgoing { tx: out_tx };
        let (connection, ctx) = Connection::bind(input, output, plugin, handler.clone(), surfaces)
            .await
            .map_err(|e| e.to_string())?;
        handler.on_ready(&ctx).await;
        let closed = connection.closed().await;
        drain.abort();
        closed.map_err(|e| e.to_string())
    });
    // 会话结束即收掉运行时线程：残留任务不再有宿主可交互
    runtime.shutdown_timeout(Duration::from_millis(200));
    result
}

/// panic 载荷的可读化：`&str` / `String` 直接取，其余类型只记类型名（不猜内容）。
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "（载荷不可读）".to_string()
}

fn cstr(ptr: *const std::ffi::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok().map(str::to_string)
}

/// 导出一个插件：四个符号一次生成，插件 crate 只需给出处理器构造表达式。
/// 插件 crate 须声明 `crate-type = ["cdylib"]`。
#[macro_export]
macro_rules! export_in_process_plugin {
    ($ctor:expr) => {
        #[no_mangle]
        pub unsafe extern "C" fn atelyx_plugin_create(
            api: *const $crate::abi::HostApi,
        ) -> $crate::abi::PluginHandle {
            $crate::in_process::serve(api, $ctor)
        }

        #[no_mangle]
        pub unsafe extern "C" fn atelyx_plugin_send(
            handle: $crate::abi::PluginHandle,
            ptr: *const u8,
            len: usize,
        ) -> u32 {
            $crate::in_process::deliver(handle, ptr, len)
        }

        #[no_mangle]
        pub unsafe extern "C" fn atelyx_plugin_draw(
            handle: $crate::abi::PluginHandle,
            target: *const $crate::abi::DrawTarget,
            sink: *const $crate::abi::DrawSink,
        ) -> u32 {
            $crate::in_process::draw_surface(handle, target, sink)
        }

        #[no_mangle]
        pub unsafe extern "C" fn atelyx_plugin_destroy(
            handle: $crate::abi::PluginHandle,
        ) -> u32 {
            $crate::in_process::shutdown(handle)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 输入回调占着该面时，绘制必须当场放弃而不是排队等——等就等于把宿主重绘线程挂在插件上。
    #[tokio::test]
    async fn draw_gives_up_while_the_surface_is_held() {
        let surfaces = Surfaces::default();
        let held = surfaces.lock("panel").await;
        assert!(surfaces.try_lock("panel").is_none(), "被占用时应放弃这一拍");
        drop(held);
        assert!(surfaces.try_lock("panel").is_some(), "空下来即恢复");
    }

    /// 面各自独立：一个面被占用不影响另一个面这一拍的绘制。
    #[tokio::test]
    async fn surfaces_are_independent() {
        let surfaces = Surfaces::default();
        let _held = surfaces.lock("panel").await;
        assert!(surfaces.try_lock("inspector").is_some());
    }
}
