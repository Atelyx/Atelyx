//! 系统托盘：常驻图标承载宿主条目与插件条目，用户选择经事件广播回 Elm。
//! 托盘回调只投给创建图标的线程，故 Windows 上图标与消息循环都在专用线程；其余平台如实报不可用。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::broadcast;

use atelyx_core::plugin_runtime::TrayItemSpec;

/// 事件广播容量：消费者落后只丢事件（托盘点击不补投）。
const EVENT_BUFFER: usize = 16;

/// 托盘隐藏窗口的类名（单实例启动时按它找已运行实例的唤出窗口）。
#[cfg(windows)]
pub const WINDOW_CLASS: &str = "AtelyxTray";

/// 托盘选择去向：插件条目交插件自己，宿主条目由壳处理。
/// 事件只在 Windows 托盘线程里产生，其余平台保留类型让调用方不分叉。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub enum TrayEvent {
    PluginClick { plugin_id: String, item_id: String },
    /// 唤出主窗口（托盘条目与第二个进程的唤出消息共用）。
    ShowMain,
    /// 打开主窗口并切到设置页。
    OpenSettings,
    Quit,
    /// 图标创建失败（Explorer 未就绪 / 托盘区不可用）：壳侧据此给出可见提示，
    /// 托盘线程按定时器重试，下一次条目提交也会重试。
    Unavailable(String),
}

/// 托盘服务（随 `App` 生命周期，进程内单实例）。
pub struct Service {
    worker: Option<platform::Worker>,
    events: Arc<broadcast::Sender<TrayEvent>>,
    /// 图标是否已挂到系统托盘（托盘线程写、壳读）。
    visible: Arc<AtomicBool>,
    /// 不可用原因（`None` = 平台支持且线程已就绪）。
    unavailable: Option<String>,
}

impl Service {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let events = Arc::new(events);
        let visible = Arc::new(AtomicBool::new(false));
        match platform::spawn(events.clone(), visible.clone()) {
            Ok(worker) => Self { worker: Some(worker), events, visible, unavailable: None },
            Err(reason) => Self { worker: None, events, visible, unavailable: Some(reason) },
        }
    }

    /// 托盘线程是否就绪（平台不支持托盘时为 false）。
    pub fn available(&self) -> bool {
        self.worker.is_some()
    }

    /// 图标当前是否在系统托盘里（创建失败或尚未建起为 false）。
    pub fn icon_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// 托盘不可用的原因（`None` = 线程已就绪）。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// 整发整换某插件的托盘条目（空清单 = 收起该插件的条目）；被接受才返回成功。
    pub fn set_items(&self, plugin_id: &str, items: &[TrayItemSpec]) -> Result<(), String> {
        let Some(worker) = &self.worker else {
            return Err(match &self.unavailable {
                Some(reason) => format!("系统托盘不可用：{reason}"),
                None => "系统托盘不可用".to_string(),
            });
        };
        worker
            .send(platform::Cmd::SetItems {
                plugin_id: plugin_id.to_string(),
                items: items.to_vec(),
            })
            .map_err(|reason| format!("系统托盘不可用：{reason}"))
    }

    /// 插件停用 / 卸载时摘掉它的条目（幂等；发送失败即托盘线程已收工，无需补救）。
    pub fn remove_plugin(&self, plugin_id: &str) {
        if let Some(worker) = &self.worker {
            let _ = worker.send(platform::Cmd::RemovePlugin { plugin_id: plugin_id.to_string() });
        }
    }

    /// 订阅标识：同一服务多次订阅共享一条广播（`run_with` 据此只建一条流）。
    pub fn spec(&self) -> EventSpec {
        EventSpec(self.events.clone())
    }
}

/// 事件源标识（哈希按广播通道指针：服务换了才重建流）。
#[derive(Clone)]
pub struct EventSpec(Arc<broadcast::Sender<TrayEvent>>);

impl std::hash::Hash for EventSpec {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// 托盘事件订阅流：把托盘线程的广播接进 Elm update。
pub fn events_stream(spec: &EventSpec) -> impl iced::futures::Stream<Item = TrayEvent> + use<> {
    let mut events = spec.0.subscribe();
    iced::stream::channel(8, async move |mut output| loop {
        match events.recv().await {
            Ok(event) => {
                if iced::futures::SinkExt::send(&mut output, event).await.is_err() {
                    break;
                }
            }
            // 落后只丢中间点击；关闭即服务收尾
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    })
}

#[cfg(windows)]
mod platform {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::mem::size_of;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, OnceLock};
    use std::thread::JoinHandle;

    use atelyx_core::plugin_runtime::TrayItemSpec;
    use tokio::sync::broadcast;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, GetMessageW, HMENU, KillTimer, LoadIconW, LoadImageW,
        PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW, RegisterWindowMessageW,
        SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage, HICON, IDI_APPLICATION,
        IMAGE_ICON, LR_DEFAULTSIZE, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
        TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
        WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
    };

    use super::TrayEvent;

    /// 托盘回调消息（只在本线程窗口上出现）。
    const CALLBACK: u32 = WM_APP + 1;
    /// 唤醒消息：命令经线程消息叫醒停在 `GetMessageW` 上的循环。
    const WAKE: u32 = WM_APP + 2;
    /// 图标标识（单图标；宿主与插件条目共用一个）。
    const ICON_ID: u32 = 1;
    /// 图标建失败后的重试定时器与节拍（毫秒）。
    const RETRY_TIMER: usize = 1;
    const RETRY_INTERVAL_MS: u32 = 2000;
    /// 托盘提示文字。
    const TIP: &str = "Atelyx";

    pub(super) enum Cmd {
        SetItems { plugin_id: String, items: Vec<TrayItemSpec> },
        RemovePlugin { plugin_id: String },
        Shutdown,
    }

    pub(super) struct Worker {
        tx: Sender<Cmd>,
        /// 工作线程 id（投唤醒消息用）。
        thread_id: u32,
        /// 工作线程句柄（收工时等它把图标摘掉再返回）。
        handle: Option<JoinHandle<()>>,
    }

    impl Worker {
        pub(super) fn send(&self, cmd: Cmd) -> Result<(), String> {
            self.tx
                .send(cmd)
                .map_err(|_| "托盘线程已结束".to_string())?;
            wake(self.thread_id);
            Ok(())
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            let _ = self.tx.send(Cmd::Shutdown);
            wake(self.thread_id);
            // 图标删除在托盘线程上做：不等它就退出会留下点不到的幽灵条目
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// 起托盘线程并等它交出线程 id（窗口建不起来即如实失败；图标建不起来由共享标志表达）。
    pub(super) fn spawn(
        events: Arc<broadcast::Sender<TrayEvent>>,
        visible: Arc<AtomicBool>,
    ) -> Result<Worker, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let handle = std::thread::spawn(move || run(rx, ready_tx, events, visible));
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Worker { tx, thread_id, handle: Some(handle) }),
            Ok(Err(reason)) => Err(reason),
            Err(_) => Err("托盘线程未能启动".to_string()),
        }
    }

    /// 托盘线程自有状态（条目表 + 图标句柄），只在该线程上读写。
    struct State {
        hwnd: HWND,
        icon: HICON,
        /// 图标是否已挂上（共享给壳判断关窗能否隐藏）。
        visible: Arc<AtomicBool>,
        /// 各插件的条目表（按插件 id 排序，菜单分栏顺序稳定）；清单为空即整条移除。
        plugins: BTreeMap<String, Vec<TrayItemSpec>>,
        events: Arc<broadcast::Sender<TrayEvent>>,
        /// 本轮失败是否已上报：同一次失败不反复提示，建成功即复位。
        failure_reported: bool,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    fn run(
        rx: Receiver<Cmd>,
        ready: Sender<Result<u32, String>>,
        events: Arc<broadcast::Sender<TrayEvent>>,
        visible: Arc<AtomicBool>,
    ) {
        let hwnd = match create_window() {
            Ok(hwnd) => hwnd,
            Err(e) => {
                let _ = ready.send(Err(format!("创建托盘窗口失败（{e}）")));
                return;
            }
        };
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                hwnd,
                icon: load_icon(),
                visible,
                plugins: BTreeMap::new(),
                events,
                failure_reported: false,
            })
        });
        // 图标常驻：不等插件条目，建不起来由定时器继续重试
        STATE.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                sync_icon(state);
            }
        });
        if ready.send(Ok(current_thread_id())).is_err() {
            STATE.with(|state| {
                if let Some(mut state) = state.borrow_mut().take() {
                    delete_icon(&mut state);
                }
            });
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        }
        loop {
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    Cmd::Shutdown => {
                        STATE.with(|state| {
                            if let Some(mut state) = state.borrow_mut().take() {
                                delete_icon(&mut state);
                            }
                        });
                        unsafe {
                            let _ = DestroyWindow(hwnd);
                        }
                        return;
                    }
                    cmd => STATE.with(|state| {
                        if let Some(state) = state.borrow_mut().as_mut() {
                            apply(state, cmd);
                        }
                    }),
                }
            }
            // 托盘回调与唤醒消息都经本线程队列到达，阻塞等消息即可
            let mut msg = MSG::default();
            let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if got.0 < 0 {
                // 取消息失败：摘掉图标再收工，免留一个点不到的幽灵条目
                STATE.with(|state| {
                    if let Some(mut state) = state.borrow_mut().take() {
                        delete_icon(&mut state);
                    }
                });
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return;
            }
            if got.0 == 0 {
                return;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn apply(state: &mut State, cmd: Cmd) {
        match cmd {
            Cmd::SetItems { plugin_id, items } => {
                if items.is_empty() {
                    state.plugins.remove(&plugin_id);
                } else {
                    state.plugins.insert(plugin_id, items);
                }
                sync_icon(state);
            }
            Cmd::RemovePlugin { plugin_id } => {
                if state.plugins.remove(&plugin_id).is_some() {
                    sync_icon(state);
                }
            }
            Cmd::Shutdown => {}
        }
    }

    /// 图标常驻：没挂上就建一次；失败上报一次并起重试定时器。
    fn sync_icon(state: &mut State) {
        if state.visible.load(Ordering::Relaxed) {
            return;
        }
        // 建失败报给壳（发布态没有 stderr），由定时器继续重试
        if unsafe { Shell_NotifyIconW(NIM_ADD, &icon_data(state)) }.as_bool() {
            state.visible.store(true, Ordering::Relaxed);
            state.failure_reported = false;
            unsafe {
                let _ = KillTimer(Some(state.hwnd), RETRY_TIMER);
            }
        } else {
            if !state.failure_reported {
                state.failure_reported = true;
                let _ = state
                    .events
                    .send(TrayEvent::Unavailable("图标创建失败，稍后自动重试".to_string()));
            }
            unsafe {
                let _ = SetTimer(Some(state.hwnd), RETRY_TIMER, RETRY_INTERVAL_MS, None);
            }
        }
    }

    fn delete_icon(state: &mut State) {
        if !state.visible.load(Ordering::Relaxed) {
            return;
        }
        let data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: state.hwnd,
            uID: ICON_ID,
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        state.visible.store(false, Ordering::Relaxed);
    }

    fn icon_data(state: &State) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: state.hwnd,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: CALLBACK,
            hIcon: state.icon,
            ..Default::default()
        };
        let tip: Vec<u16> = TIP.encode_utf16().take(data.szTip.len() - 1).collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        data
    }

    /// 应用图标（构建期以资源 id 1 编入 exe）；取不到退回系统默认图标。
    fn load_icon() -> HICON {
        let instance = unsafe { GetModuleHandleW(None) }
            .map(|module| HINSTANCE(module.0))
            .ok();
        let embedded =
            unsafe { LoadImageW(instance, PCWSTR(1 as *const u16), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE) };
        embedded
            .map(|handle| HICON(handle.0))
            .unwrap_or_else(|_| unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default())
    }

    fn create_window() -> windows::core::Result<HWND> {
        let class = wide(super::WINDOW_CLASS);
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }?.0);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&wc) } == 0 {
            return Err(windows::core::Error::from_thread());
        }
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(class.as_ptr()),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            CALLBACK => {
                // 托盘回调把鼠标消息塞在 lParam 低字
                let mouse = lparam.0 as u32 & 0xFFFF;
                match mouse {
                    // 左键单击不进菜单，直接唤出工作区；菜单走右键与上下文菜单
                    WM_LBUTTONUP => STATE.with(|cell| {
                        let Ok(borrowed) = cell.try_borrow() else { return };
                        let Some(state) = borrowed.as_ref() else { return };
                        if state.visible.load(Ordering::Relaxed) {
                            let _ = state.events.send(TrayEvent::ShowMain);
                        }
                    }),
                    // 菜单是嵌套消息循环：此时不能持锁，取不到状态就忽略这次点击
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(),
                    _ => {}
                }
                LRESULT(0)
            }
            // Explorer 重启后托盘区清空：按现有条目把图标重挂一遍。
            // 菜单是模态消息循环，期间广播消息会重入这里——状态被借走就跳过这次重建
            _ if is_registered_message(msg, taskbar_created()) => {
                STATE.with(|cell| {
                    let Ok(mut borrowed) = cell.try_borrow_mut() else { return };
                    let Some(state) = borrowed.as_mut() else { return };
                    state.visible.store(false, Ordering::Relaxed);
                    sync_icon(state);
                });
                LRESULT(0)
            }
            // 第二个进程请求唤出主窗口：走与托盘条目同一条事件
            _ if is_registered_message(msg, show_request()) => {
                STATE.with(|cell| {
                    let Ok(mut borrowed) = cell.try_borrow_mut() else { return };
                    let Some(state) = borrowed.as_mut() else { return };
                    let _ = state.events.send(TrayEvent::ShowMain);
                });
                LRESULT(0)
            }
            WM_TIMER => {
                if wparam.0 == RETRY_TIMER {
                    STATE.with(|cell| {
                        let Ok(mut borrowed) = cell.try_borrow_mut() else { return };
                        let Some(state) = borrowed.as_mut() else { return };
                        sync_icon(state);
                    });
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// 注册消息匹配：注册失败返回 0，不能与 `WM_NULL` 等任何消息相等。
    fn is_registered_message(msg: u32, id: u32) -> bool {
        id != 0 && msg == id
    }

    /// 单实例唤出消息 id（注册一次全进程同值）。
    fn show_request() -> u32 {
        static ID: OnceLock<u32> = OnceLock::new();
        *ID.get_or_init(|| {
            let name = wide(crate::single_instance::SHOW_MESSAGE);
            unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) }
        })
    }

    /// Explorer 广播的「任务栏已创建」消息 id（注册一次全进程同值，按窗口消息热路径缓存）。
    fn taskbar_created() -> u32 {
        static ID: OnceLock<u32> = OnceLock::new();
        *ID.get_or_init(|| unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) })
    }

    /// 弹菜单并处理选择：插件条目按插件分栏（分隔线隔开），宿主条目恒在末尾。
    /// 带 children 的插件条目是二级子菜单：悬停展开时主菜单保持打开，选择仍按条目 id 回发。
    fn show_menu() {
        STATE.with(|cell| {
            // 菜单是嵌套消息循环：重入时取不到状态就忽略这次点击
            let Ok(mut state) = cell.try_borrow_mut() else { return };
            let Some(state) = state.as_mut() else { return };
            // 图标还没挂上（创建失败 / Explorer 未就绪）时回调可能是残留的：不弹空菜单
            if !state.visible.load(Ordering::Relaxed) {
                return;
            }
            let mut point = POINT::default();
            if unsafe { GetCursorPos(&mut point) }.is_err() {
                return;
            }
            let Ok(menu) = (unsafe { CreatePopupMenu() }) else { return };
            let mut next_id = 1u32;
            let mut targets: Vec<(u32, TrayEvent)> = Vec::new();
            for (index, (plugin_id, items)) in state.plugins.iter().enumerate() {
                if index > 0 {
                    unsafe {
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                    }
                }
                append_plugin_items(menu, plugin_id, items, &mut next_id, &mut targets);
            }
            if !state.plugins.is_empty() {
                unsafe {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
            }
            for (label, event) in [
                ("打开工作区", TrayEvent::ShowMain),
                ("设置", TrayEvent::OpenSettings),
                ("退出", TrayEvent::Quit),
            ] {
                let text = wide(label);
                unsafe {
                    let _ = AppendMenuW(menu, MF_STRING, next_id as usize, PCWSTR(text.as_ptr()));
                }
                targets.push((next_id, event));
                next_id += 1;
            }
            // 前台化 + 菜单结束后补一条空消息：否则点菜单外部菜单不收起（托盘菜单的既定配合）
            unsafe {
                let _ = SetForegroundWindow(state.hwnd);
            }
            let selected = unsafe {
                TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                    point.x,
                    point.y,
                    None,
                    state.hwnd,
                    None,
                )
            }
            .0 as u32;
            unsafe {
                let _ = DestroyMenu(menu);
                let _ = PostMessageW(Some(state.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            }
            if let Some((_, event)) = targets.into_iter().find(|(id, _)| *id == selected) {
                let _ = state.events.send(event);
            }
        });
    }

    /// 追加一个插件的条目：带 children 的条目展开为二级子菜单（整棵菜单只有这一层）。
    fn append_plugin_items(
        menu: HMENU,
        plugin_id: &str,
        items: &[TrayItemSpec],
        next_id: &mut u32,
        targets: &mut Vec<(u32, TrayEvent)>,
    ) {
        for item in items {
            let label = menu_label(&item.label);
            let enabled = item.enabled != Some(false);
            if item.children.is_empty() {
                append_leaf(menu, plugin_id, item, enabled, label, next_id, targets);
                continue;
            }
            let Ok(submenu) = (unsafe { CreatePopupMenu() }) else { continue };
            for child in &item.children {
                let child_label = menu_label(&child.label);
                let child_enabled = child.enabled != Some(false);
                append_leaf(
                    submenu,
                    plugin_id,
                    child,
                    child_enabled,
                    child_label,
                    next_id,
                    targets,
                );
            }
            let flags = if enabled { MF_POPUP } else { MF_POPUP | MF_GRAYED };
            unsafe {
                let _ = AppendMenuW(menu, flags, submenu.0 as usize, PCWSTR(label.as_ptr()));
            }
        }
    }

    /// 追加一个可点击条目（命令 id 已是全菜单唯一的）。
    fn append_leaf(
        menu: HMENU,
        plugin_id: &str,
        item: &TrayItemSpec,
        enabled: bool,
        label: Vec<u16>,
        next_id: &mut u32,
        targets: &mut Vec<(u32, TrayEvent)>,
    ) {
        let flags = if enabled { MF_STRING } else { MF_STRING | MF_GRAYED };
        unsafe {
            let _ = AppendMenuW(menu, flags, *next_id as usize, PCWSTR(label.as_ptr()));
        }
        targets.push((
            *next_id,
            TrayEvent::PluginClick {
                plugin_id: plugin_id.to_string(),
                item_id: item.id.clone(),
            },
        ));
        *next_id += 1;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 插件文字按原样显示：`&` 是助记符前缀、`\t` 是右对齐分隔、换行会截断条目，逐个转义。
    fn menu_label(text: &str) -> Vec<u16> {
        let mut out = Vec::with_capacity(text.len() + 1);
        for c in text.chars() {
            match c {
                '&' => out.extend([u16::from(b'&'), u16::from(b'&')]),
                '\t' | '\r' | '\n' => out.push(u16::from(b' ')),
                c => {
                    let mut buf = [0u16; 2];
                    out.extend_from_slice(c.encode_utf16(&mut buf));
                }
            }
        }
        out.push(0);
        out
    }

    /// 把工作线程从消息循环里叫醒（命令先入通道，再投一条线程消息）。
    fn wake(thread_id: u32) {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WAKE, WPARAM(0), LPARAM(0));
        }
    }

    fn current_thread_id() -> u32 {
        unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::sync::Arc;

    use atelyx_core::plugin_runtime::TrayItemSpec;
    use tokio::sync::broadcast;

    use super::TrayEvent;

    /// 桩只为对齐 Windows 侧的调用形状，字段无人读取。
    #[allow(dead_code)]
    pub(super) enum Cmd {
        SetItems { plugin_id: String, items: Vec<TrayItemSpec> },
        RemovePlugin { plugin_id: String },
    }

    pub(super) struct Worker;

    impl Worker {
        pub(super) fn send(&self, _cmd: Cmd) -> Result<(), String> {
            Err("当前平台未提供托盘实现".to_string())
        }
    }

    pub(super) fn spawn(
        _events: Arc<broadcast::Sender<TrayEvent>>,
        _visible: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Worker, String> {
        Err("当前平台未提供托盘实现".to_string())
    }
}
