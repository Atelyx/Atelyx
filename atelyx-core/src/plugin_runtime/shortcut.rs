//! `shortcuts` 能力面：OS 级全局快捷键的注册、归属仲裁与触发转发。
//! OS 管理器与它（Windows 上）的消息窗、消息循环都建在专用工作线程上：`RegisterHotKey` 只接受
//! **属于调用线程**的窗口，插件读线程直接调会拿到 ERROR_WINDOW_OF_OTHER_THREAD，故注册 / 注销一律
//! 转投该线程执行并等回执（插件侧要同步结果，被占用必须可见失败）。
//! 触发事件在 OS 消息线程上到达，经派发线程转投插件：写插件 stdin 可能阻塞，不得卡住消息线程。

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use atelyx_plugin_protocol::method::ShortcutTriggeredParams;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// 触发转发回调：`(插件 id, 注册串)`。
pub(crate) type TriggerSink = Arc<dyn Fn(String, String) + Send + Sync>;

/// 归属条目：注册串随插件留档，触发回查与整体释放都要用。
#[derive(Clone)]
struct Owner {
    plugin_id: String,
    accelerator: String,
}

/// 归属表（纯逻辑，可单测）：占用仲裁按 OS 快捷键 id 判定——id 与修饰键书写顺序无关，
/// 按原始串判会让 `Shift+Alt+E` 与 `Alt+Shift+E` 的二次注册互相当成不同键。
#[derive(Default)]
struct Owners {
    by_id: HashMap<u32, Owner>,
}

impl Owners {
    /// 登记归属：`Ok(true)` = 新增，`Ok(false)` = 本插件重复注册（幂等，登记串更新为这一次的写法），
    /// `Err` = 已被其他插件占用。触发上报的是登记串，故同一组键换写法重注册后必须报最新串。
    fn acquire(&mut self, hotkey: HotKey, accelerator: &str, plugin_id: &str) -> Result<bool, String> {
        if let Some(existing) = self.by_id.get_mut(&hotkey.id()) {
            if existing.plugin_id != plugin_id {
                return Err(format!("快捷键 {accelerator} 已被插件 {} 注册", existing.plugin_id));
            }
            if existing.accelerator != accelerator {
                existing.accelerator = accelerator.to_string();
            }
            return Ok(false);
        }
        self.by_id.insert(
            hotkey.id(),
            Owner { plugin_id: plugin_id.to_string(), accelerator: accelerator.to_string() },
        );
        Ok(true)
    }

    fn get(&self, id: u32) -> Option<Owner> {
        self.by_id.get(&id).cloned()
    }

    /// 摘除本插件在该 id 上的归属（非本插件所有即不动作）。
    fn release(&mut self, id: u32, plugin_id: &str) -> Option<Owner> {
        match self.by_id.get(&id) {
            Some(owner) if owner.plugin_id == plugin_id => self.by_id.remove(&id),
            _ => None,
        }
    }

    /// 回滚一条注销失败的登记（仅在该 id 仍空闲时放回，不覆盖期间的新归属）。
    fn reinsert_if_absent(&mut self, id: u32, owner: Owner) {
        self.by_id.entry(id).or_insert(owner);
    }

    fn take_plugin(&mut self, plugin_id: &str) -> Vec<Owner> {
        let ids: Vec<u32> = self
            .by_id
            .iter()
            .filter(|(_, owner)| owner.plugin_id == plugin_id)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter().filter_map(|id| self.by_id.remove(&id)).collect()
    }
}

/// 交给 OS 工作线程执行的请求；注册 / 注销 / 试探带一条回执通道（插件侧要同步结果）。
enum WorkerRequest {
    Register(HotKey, Sender<Result<(), String>>),
    Unregister(HotKey, Sender<Result<(), String>>),
    /// 试探某键位是否可注册：注册成功即空闲（随即注销），失败即被占用。
    Probe(HotKey, Sender<Result<bool, String>>),
    Shutdown,
}

/// OS 工作线程句柄。
struct OsWorker {
    tx: Sender<WorkerRequest>,
    /// 工作线程 id（Windows 上投唤醒消息用，见 [`wake_worker`]）。
    thread_id: u32,
}

impl Drop for OsWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(WorkerRequest::Shutdown);
        wake_worker(self.thread_id);
    }
}

/// 全局快捷键注册器（宿主运行时内单例，多窗口共享）。
pub(crate) struct Registrar {
    owners: Arc<Mutex<Owners>>,
    /// OS 工作线程；`None` = 本平台 / 本会话不可用（注册一律以可读失败收场）。
    os: Option<OsWorker>,
    /// 不可用原因（`None` = OS 层可用）。
    unavailable: Option<String>,
    sink: Arc<Mutex<Option<TriggerSink>>>,
    /// 派发线程只起一次（首次真正注册时起——从不注册的会话不必背一条常驻线程）。
    dispatcher: std::sync::Once,
}

impl Registrar {
    /// 建立注册器：起 OS 工作线程并在其上建管理器；等它交出线程 id 才算可用（初始化失败如实回报）。
    pub(crate) fn new() -> Arc<Self> {
        if let Err(reason) = os_support() {
            return Self::unavailable(reason);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || os_worker(rx, ready_tx));
        let thread_id = match ready_rx.recv() {
            Ok(Ok(thread_id)) => thread_id,
            Ok(Err(reason)) => return Self::unavailable(reason),
            Err(_) => return Self::unavailable("全局快捷键不可用：OS 工作线程未能启动"),
        };
        Arc::new(Self {
            owners: Arc::new(Mutex::new(Owners::default())),
            os: Some(OsWorker { tx, thread_id }),
            unavailable: None,
            sink: Arc::new(Mutex::new(None)),
            dispatcher: std::sync::Once::new(),
        })
    }

    /// 无 OS 层的注册器（平台 / 会话不支持时用）：注册一律可读失败，注销是幂等空操作。
    pub(crate) fn unavailable(reason: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            owners: Arc::new(Mutex::new(Owners::default())),
            os: None,
            unavailable: Some(reason.into()),
            sink: Arc::new(Mutex::new(None)),
            dispatcher: std::sync::Once::new(),
        })
    }

    /// 起触发派发线程（只起一次）。
    fn ensure_dispatcher(&self) {
        let owners = self.owners.clone();
        let sink = self.sink.clone();
        self.dispatcher.call_once(|| spawn_dispatcher(owners, sink));
    }

    /// 装上触发转发回调（运行时装上；未装上时到达的触发直接丢弃）。
    pub(crate) fn set_sink(&self, sink: TriggerSink) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    /// 注册全局快捷键（归属插件仅用于占用仲裁与随插件整体释放）。
    /// 同插件重复注册幂等成功且登记串取最新一次；被其他插件占用、格式非法或 OS 注册失败
    /// （被系统 / 其他应用占用）均返回可读错误。
    pub(crate) fn register(&self, plugin_id: &str, accelerator: &str) -> Result<(), String> {
        if let Some(reason) = &self.unavailable {
            return Err(reason.clone());
        }
        let hotkey = parse(accelerator)?;
        let fresh = self.owners.lock().unwrap().acquire(hotkey, accelerator, plugin_id)?;
        if !fresh {
            return Ok(());
        }
        // 派发线程先就位再落 OS 注册：注册成功那一刻起就可能来触发，不能有一段无人接的空窗
        self.ensure_dispatcher();
        // 登记先行、OS 注册失败即回滚：不留「登记在册、OS 未注册」的假占用
        if let Err(e) = self.os_register(hotkey) {
            self.owners.lock().unwrap().release(hotkey.id(), plugin_id);
            return Err(format!("全局快捷键注册失败（{accelerator}）：{e}"));
        }
        Ok(())
    }

    /// 事前查询某键位是否可注册：宿主已知归属时直接答（含占用方插件 id），否则探一次 OS
    /// （注册成功即空闲、随即注销；被系统 / 其他应用占用即不可用，占用者身份无从得知）。
    /// 返回 `(available, owner)`——`owner` 仅在本进程内有归属时给出。
    pub(crate) fn query(&self, plugin_id: &str, accelerator: &str) -> Result<(bool, Option<String>), String> {
        let hotkey = parse(accelerator)?;
        if let Some(owner) = self.owners.lock().unwrap().get(hotkey.id()) {
            return Ok((owner.plugin_id == plugin_id, Some(owner.plugin_id)));
        }
        if let Some(reason) = &self.unavailable {
            return Err(reason.clone());
        }
        Ok((self.os_probe(hotkey)?, None))
    }

    /// 试探一次 OS 层占用（在 OS 工作线程上注册后立刻注销）。
    fn os_probe(&self, hotkey: HotKey) -> Result<bool, String> {
        let Some(worker) = &self.os else {
            return Ok(true);
        };
        let (tx, rx) = std::sync::mpsc::channel();
        if worker.tx.send(WorkerRequest::Probe(hotkey, tx)).is_err() {
            return Err("全局快捷键不可用：OS 工作线程已结束".to_string());
        }
        wake_worker(worker.thread_id);
        rx.recv()
            .unwrap_or_else(|_| Err("全局快捷键不可用：OS 工作线程已结束".to_string()))
    }

    /// 注销本插件注册的单个快捷键；未注册即幂等成功，非本插件注册的可读拒绝。
    pub(crate) fn unregister(&self, plugin_id: &str, accelerator: &str) -> Result<(), String> {
        let hotkey = parse(accelerator)?;
        // 不可用时从未注册过任何东西：注销是纯粹的幂等空操作
        if self.unavailable.is_some() {
            return Ok(());
        }
        let removed = {
            let mut owners = self.owners.lock().unwrap();
            match owners.get(hotkey.id()) {
                Some(owner) if owner.plugin_id != plugin_id => {
                    return Err(format!(
                        "快捷键 {accelerator} 由插件 {} 注册，无权注销",
                        owner.plugin_id
                    ));
                }
                Some(_) => owners.release(hotkey.id(), plugin_id),
                None => None,
            }
        };
        let Some(owner) = removed else {
            return Ok(());
        };
        // 先出册再注销 OS 层：注销期间到达的触发查不到归属，自然丢弃；OS 注销失败则把登记放回
        if let Err(e) = self.os_unregister(hotkey) {
            self.owners.lock().unwrap().reinsert_if_absent(hotkey.id(), owner);
            return Err(format!("全局快捷键注销失败（{accelerator}）：{e}"));
        }
        Ok(())
    }

    /// 按插件整体注销（停用 / 卸载 / 失败收尾）。注销失败的条目放回登记并把原因写入诊断输出。
    pub(crate) fn release_plugin(&self, plugin_id: &str) {
        let taken = self.owners.lock().unwrap().take_plugin(plugin_id);
        for owner in taken {
            let Ok(hotkey) = parse(&owner.accelerator) else { continue };
            if let Err(e) = self.os_unregister(hotkey) {
                eprintln!(
                    "[plugin {plugin_id}] 全局快捷键注销失败（{}）：{e}",
                    owner.accelerator
                );
                self.owners.lock().unwrap().reinsert_if_absent(hotkey.id(), owner);
            }
        }
    }

    fn os_register(&self, hotkey: HotKey) -> Result<(), String> {
        self.os_call(|reply| WorkerRequest::Register(hotkey, reply))
    }

    fn os_unregister(&self, hotkey: HotKey) -> Result<(), String> {
        self.os_call(|reply| WorkerRequest::Unregister(hotkey, reply))
    }

    /// 把一次管理动作转投 OS 工作线程并等回执（注册 / 注销必须在管理器所在线程上执行）。
    fn os_call(
        &self,
        request: impl FnOnce(Sender<Result<(), String>>) -> WorkerRequest,
    ) -> Result<(), String> {
        let Some(worker) = &self.os else {
            return Ok(());
        };
        let (tx, rx) = std::sync::mpsc::channel();
        if worker.tx.send(request(tx)).is_err() {
            return Err("全局快捷键不可用：OS 工作线程已结束".to_string());
        }
        wake_worker(worker.thread_id);
        // 工作线程只在收到收工指令后退出；回执通道断开即它已收工，按可读失败上抛
        rx.recv()
            .unwrap_or_else(|_| Err("全局快捷键不可用：OS 工作线程已结束".to_string()))
    }
}

/// OS 工作线程：管理器（Windows 上含其消息窗）与消息循环都在本线程建立——见模块头的线程约束。
fn os_worker(rx: Receiver<WorkerRequest>, ready: Sender<Result<u32, String>>) {
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(e) => {
            let _ = ready.send(Err(format!("全局快捷键不可用：OS 注册器初始化失败（{e}）")));
            return;
        }
    };
    let thread_id = current_thread_id();
    if ready.send(Ok(thread_id)).is_err() {
        return;
    }
    loop {
        while let Ok(request) = rx.try_recv() {
            if !handle_worker_request(&manager, request) {
                return;
            }
        }
        // Windows 靠消息循环驱动（`RegisterHotKey` 的 WM_HOTKEY 也要在这里派发）；
        // 其它平台的管理器自带连接线程，这里阻塞等请求即可
        if !wait_next_event(&rx) {
            return;
        }
    }
}

/// 处理一条工作线程请求；返回 false = 收工。
fn handle_worker_request(manager: &GlobalHotKeyManager, request: WorkerRequest) -> bool {
    match request {
        WorkerRequest::Register(hotkey, reply) => {
            let _ = reply.send(manager.register(hotkey).map_err(|e| e.to_string()));
            true
        }
        WorkerRequest::Unregister(hotkey, reply) => {
            let _ = reply.send(manager.unregister(hotkey).map_err(|e| e.to_string()));
            true
        }
        WorkerRequest::Probe(hotkey, reply) => {
            // 注册成功即空闲（立刻注销还回去）；失败即被系统 / 其他应用占用
            let free = match manager.register(hotkey) {
                Ok(()) => {
                    if let Err(e) = manager.unregister(hotkey) {
                        // 探测用的注销失败会留下一枚不属于归属表的占用：必须留痕，便于排障
                        eprintln!("[plugin] 快捷键探测注销失败（{}）：{e}", hotkey);
                    }
                    true
                }
                Err(_) => false,
            };
            let _ = reply.send(Ok(free));
            true
        }
        WorkerRequest::Shutdown => false,
    }
}

/// Windows：跑一轮消息循环直到有事件可处理（管理器消息窗的 WM_HOTKEY 在这里被派发）；
/// 返回 false = 消息循环结束（WM_QUIT / 取消息失败），工作线程随之收工。
#[cfg(windows)]
fn wait_next_event(_rx: &Receiver<WorkerRequest>) -> bool {
    use winapi::um::winuser::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // 阻塞等一条消息：唤醒消息（[`wake_worker`] 投的线程消息）也是消息，故请求不会等空转
    let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
    if got <= 0 {
        return false;
    }
    if msg.hwnd.is_null() {
        // 线程消息（我们自己的唤醒码）：无需派发，回到顶部处理请求
        return true;
    }
    unsafe {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    true
}

/// 其它平台：阻塞等一条请求（管理器自带连接线程，无需本线程泵消息）。
#[cfg(not(windows))]
fn wait_next_event(rx: &Receiver<WorkerRequest>) -> bool {
    rx.recv().is_ok()
}

/// 把工作线程从消息循环里叫醒（Windows 投一条线程消息；其它平台线程阻塞在请求通道上，无需唤醒）。
#[cfg(windows)]
fn wake_worker(thread_id: u32) {
    unsafe { winapi::um::winuser::PostThreadMessageW(thread_id, wake_message(), 0, 0) };
}

#[cfg(not(windows))]
fn wake_worker(_thread_id: u32) {}

/// 唤醒消息码：`WM_APP + 1`（`WM_APP` 起的区间归应用自用）。
#[cfg(windows)]
fn wake_message() -> u32 {
    winapi::um::winuser::WM_APP + 1
}

#[cfg(windows)]
fn current_thread_id() -> u32 {
    unsafe { winapi::um::processthreadsapi::GetCurrentThreadId() }
}

#[cfg(not(windows))]
fn current_thread_id() -> u32 {
    0
}

/// 触发派发线程：OS 消息线程只把事件送进全局频道，本线程回查归属并调 sink
/// （sink 会写插件 stdin，可能阻塞，故不能放在消息线程上做）。
fn spawn_dispatcher(owners: Arc<Mutex<Owners>>, sink: Arc<Mutex<Option<TriggerSink>>>) {
    let receiver = GlobalHotKeyEvent::receiver();
    std::thread::spawn(move || loop {
        let Ok(event) = receiver.recv() else { return };
        // 只转投按下（松开是给按住态语义用的，插件面不消费）
        if event.state != HotKeyState::Pressed {
            continue;
        }
        let Some(owner) = owners.lock().unwrap().get(event.id) else { continue };
        let sink = sink.lock().unwrap().clone();
        if let Some(sink) = sink {
            sink(owner.plugin_id, owner.accelerator);
        }
    });
}

/// 本平台 / 本会话是否具备系统级全局快捷键机制。
fn os_support() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        // Wayland 没有标准全局快捷键机制，且 XWayland 下 XGrabKey 也抓不到原生 Wayland 客户端的按键；
        // 故只在纯 X11 会话上宣称可用——其余情况如实报不可用，不留「注册成功却永不触发」的假象。
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return Err("全局快捷键不可用：当前为 Wayland 会话，系统无标准全局快捷键机制".to_string());
        }
        if std::env::var_os("DISPLAY").is_none() {
            return Err("全局快捷键不可用：当前会话无 X11 显示".to_string());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Err("全局快捷键不可用：当前平台不支持".to_string())
    }
}

/// 解析加速键描述（如 `Shift+Alt+E`、`Ctrl+F5`）；未知键或形态非法给出可读原因。
fn parse(accelerator: &str) -> Result<HotKey, String> {
    accelerator
        .trim()
        .parse::<HotKey>()
        .map_err(|e| format!("快捷键格式无效（{accelerator}）：{e}"))
}

/// 触发通知负载（[`atelyx_plugin_protocol::method::shortcut::HOST_TRIGGERED`]）。
pub(crate) fn triggered_params(accelerator: &str) -> ShortcutTriggeredParams {
    ShortcutTriggeredParams { accelerator: accelerator.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotkey(accelerator: &str) -> HotKey {
        parse(accelerator).unwrap()
    }

    #[test]
    fn accelerator_parsing_accepts_common_forms_and_rejects_junk() {
        assert!(parse("Shift+Alt+E").is_ok());
        assert!(parse("ctrl+f5").is_ok());
        assert!(parse("Super+Space").is_ok());
        // 修饰键书写顺序不影响解析结果（id 相同）
        assert_eq!(hotkey("Shift+Alt+E").id(), hotkey("Alt+Shift+E").id());
        for bad in ["", "E+Shift", "Shift+Alt", "Ctrl+不认识的键"] {
            assert!(parse(bad).is_err(), "应拒绝：{bad}");
        }
    }

    #[test]
    fn acquire_arbitrates_by_id_and_is_idempotent_for_owner() {
        let mut owners = Owners::default();
        let key = hotkey("Shift+Alt+E");
        assert!(owners.acquire(key, "Shift+Alt+E", "com.a.one").unwrap(), "首次登记");
        // 同插件换一种书写（同 id）重复注册：幂等，不是新条目，但登记串更新为最新写法
        assert!(!owners.acquire(hotkey("Alt+Shift+E"), "Alt+Shift+E", "com.a.one").unwrap());
        assert_eq!(owners.get(key.id()).unwrap().accelerator, "Alt+Shift+E");
        // 其他插件来抢：可读拒绝
        let err = owners.acquire(key, "Shift+Alt+E", "com.a.two").unwrap_err();
        assert!(err.contains("com.a.one"), "{err}");
        // 归属未被抢走
        assert_eq!(owners.get(key.id()).unwrap().plugin_id, "com.a.one");
    }

    #[test]
    fn release_requires_ownership_and_take_plugin_collects_all() {
        let mut owners = Owners::default();
        let a = hotkey("Ctrl+F1");
        let b = hotkey("Ctrl+F2");
        owners.acquire(a, "Ctrl+F1", "com.a.one").unwrap();
        owners.acquire(b, "Ctrl+F2", "com.a.one").unwrap();
        owners.acquire(hotkey("Ctrl+F3"), "Ctrl+F3", "com.a.two").unwrap();
        // 非本插件所有：不动作
        assert!(owners.release(a.id(), "com.a.two").is_none());
        assert!(owners.get(a.id()).is_some());
        let taken = owners.take_plugin("com.a.one");
        assert_eq!(taken.len(), 2, "整体释放取走该插件全部条目");
        assert!(owners.get(a.id()).is_none() && owners.get(b.id()).is_none());
        // 其他插件条目不受影响
        assert_eq!(owners.get(hotkey("Ctrl+F3").id()).unwrap().plugin_id, "com.a.two");
    }

    #[test]
    fn reinsert_does_not_override_a_newer_owner() {
        let mut owners = Owners::default();
        let key = hotkey("Ctrl+F4");
        owners.acquire(key, "Ctrl+F4", "com.a.one").unwrap();
        let owner = owners.release(key.id(), "com.a.one").unwrap();
        owners.acquire(key, "Ctrl+F4", "com.a.two").unwrap();
        owners.reinsert_if_absent(key.id(), owner);
        assert_eq!(owners.get(key.id()).unwrap().plugin_id, "com.a.two");
    }

    #[test]
    fn unavailable_registrar_fails_readably_and_leaves_no_owner() {
        let registrar = Registrar::unavailable("全局快捷键不可用：测试");
        let err = registrar.register("com.a.one", "Ctrl+F9").unwrap_err();
        assert!(err.contains("不可用"), "{err}");
        // 失败不留假占用：同键随后可被另一插件注册（此处仅检查归属表为空）
        assert!(registrar.owners.lock().unwrap().get(hotkey("Ctrl+F9").id()).is_none());
        // 注销 / 整体释放是幂等空操作
        assert!(registrar.unregister("com.a.one", "Ctrl+F9").is_ok());
        registrar.release_plugin("com.a.one");
    }
}
