//! 同进程插件传输：把 ABI 输入翻成宿主协议类型，并把插件的帧 / 服务回执接回实例。
//! 帧与输入在回调返回前复制；服务与能力请求复用进程插件同一套方法名与分派。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

use atelyx_plugin_abi as abi;
use atelyx_plugin_protocol::frame::{
    FrameInput, Key, KeyAction, Modifiers, MouseButton, NamedKey, SurfaceFrame,
};

use super::Instance;

/// 动态库装载（宿主侧的 C 边界）。
mod host;

/// ABI 帧到协议帧的复制。
mod frame;

/// 装载层对外的类型：库、实例与宿主回调。
pub use host::{Host, HostCall, PluginInstance, PluginLibrary};

/// 插件回调转发到实例；实例销毁后回调自行收场（不再有可用的插件端）。
pub(super) struct InstanceHost {
    instance: Weak<Instance>,
    token: u64,
}

impl InstanceHost {
    pub(super) fn new(instance: &Arc<Instance>, token: u64) -> Self {
        Self { instance: Arc::downgrade(instance), token }
    }
}

impl Host for InstanceHost {
    fn submit_frame(&self, surface: &str, frame: SurfaceFrame) {
        if let Some(instance) = self.instance.upgrade() {
            instance.accept_abi_frame(self.token, surface, frame);
        }
    }

    fn request_redraw(&self, surface: &str) {
        if let Some(instance) = self.instance.upgrade() {
            instance.request_redraw_abi(self.token, surface);
        }
    }

    fn log(&self, level: u32, message: &str) {
        if let Some(instance) = self.instance.upgrade() {
            instance.log_abi(self.token, level, message);
        }
    }

    fn call_host(&self, call: HostCall) {
        match self.instance.upgrade() {
            Some(instance) => instance.handle_abi_call(self.token, call),
            None => call.reply(false, "插件实例已结束"),
        }
    }

    fn subscribe(&self, name: &str) {
        if let Some(instance) = self.instance.upgrade() {
            instance.subscribe_abi(self.token, name);
        }
    }

    fn reply_service(&self, call_id: u64, ok: bool, payload: &str) {
        if let Some(instance) = self.instance.upgrade() {
            instance.reply_abi_service(self.token, call_id, ok, payload);
        }
    }
}

/// 动态库会话：库在本会话内跨启停保留，插件实例随启停创建 / 销毁。
/// 库的最后一个引用随会话释放——旧实例被丢弃（换代）后才能加载同路径的新文件。
pub(super) struct AbiSession {
    library: Mutex<Option<Arc<PluginLibrary>>>,
    state: Mutex<AbiState>,
    /// 插件入口与实例销毁的串行闸，允许持闸线程在插件回调里再入。
    entry: EntryGate,
    next_service_call: AtomicU64,
}

struct AbiState {
    plugin: Option<Arc<PluginInstance>>,
    /// 实例尚未装入时到达的事件（面挂载 / 输入 / 领域事件）：装入后按序补发。
    pending: VecDeque<PendingNotice>,
    alive: bool,
    /// 本次会话的运行令牌；建实例 / 装入都按它拒绝已收场的迟到线程。
    token: u64,
    replies: HashMap<u64, HostCall>,
    service_calls: HashMap<u64, String>,
}

enum PendingNotice {
    Attach(String, u32),
    Detach(String),
    Event(String, String),
    Input(String, FrameInput),
}

/// 一次入口转投的落点：装好了 / 初始化窗口内还没装入 / 会话已结束。
#[derive(Clone, Copy)]
pub(super) enum CallState {
    Delivered,
    Starting,
    Stopped,
}

impl Default for AbiSession {
    fn default() -> Self {
        Self {
            library: Mutex::new(None),
            state: Mutex::new(AbiState {
                plugin: None,
                pending: VecDeque::new(),
                alive: false,
                token: 0,
                replies: HashMap::new(),
                service_calls: HashMap::new(),
            }),
            entry: EntryGate::default(),
            next_service_call: AtomicU64::new(1),
        }
    }
}

impl AbiSession {
    pub(super) fn library(&self) -> Option<Arc<PluginLibrary>> {
        self.library.lock().unwrap().clone()
    }

    pub(super) fn set_library(&self, library: Arc<PluginLibrary>) {
        *self.library.lock().unwrap() = Some(library);
    }

    /// 开新会话：先收掉上一次会话的残留（实例、在途回执、服务序号、待发事件），再放开新会话。
    pub(super) fn begin_session(&self, token: u64) {
        let (old, calls) = {
            let _entry = self.entry.lock();
            let mut state = self.state.lock().unwrap();
            state.alive = false;
            state.token = 0;
            state.pending.clear();
            state.service_calls.clear();
            (state.plugin.take(), take_calls(&mut state.replies))
        };
        fail_calls(calls, "插件已重载，调用已取消");
        if let Some(old) = old {
            retire(old);
        }
        let mut state = self.state.lock().unwrap();
        state.token = token;
        state.alive = true;
    }

    /// 装入本会话建出的实例；会话已收场（停用 / 换代）时销毁该实例并返回 false。
    /// 初始化期间攒下的事件在装入后按到达顺序补发。
    pub(super) fn install(&self, plugin: PluginInstance, token: u64) -> bool {
        let entry = self.entry.lock();
        let old = {
            let mut state = self.state.lock().unwrap();
            if !state.alive || state.token != token {
                drop(state);
                drop(entry);
                plugin.destroy();
                return false;
            }
            state.plugin.replace(Arc::new(plugin))
        };
        drop(entry);
        if let Some(old) = old {
            retire(old);
        }
        self.drain_pending();
        true
    }

    pub(super) fn attach(&self, surface: &str, kind: u32) {
        self.post(PendingNotice::Attach(surface.to_string(), kind));
    }

    pub(super) fn detach(&self, surface: &str) {
        self.post(PendingNotice::Detach(surface.to_string()));
    }

    pub(super) fn notify(&self, name: &str, payload: &str) {
        self.post(PendingNotice::Event(name.to_string(), payload.to_string()));
    }

    /// 事件入队后立即按序交给实例；实例尚未装入时留待装入后补发。
    fn post(&self, notice: PendingNotice) {
        let _entry = self.entry.lock();
        let installed = {
            let mut state = self.state.lock().unwrap();
            if !state.alive {
                return;
            }
            state.pending.push_back(notice);
            state.plugin.is_some()
        };
        if installed {
            self.drain_pending();
        }
    }

    fn drain_pending(&self) {
        let _entry = self.entry.lock();
        loop {
            let next = {
                let mut state = self.state.lock().unwrap();
                if !state.alive {
                    state.pending.clear();
                    return;
                }
                let Some(plugin) = state.plugin.clone() else {
                    return;
                };
                match state.pending.pop_front() {
                    Some(notice) => (plugin, notice),
                    None => return,
                }
            };
            deliver(&next.0, next.1);
        }
    }

    /// 输入与面挂载同序排队：实例尚未装入时到达的输入（典型是挂面后的首次尺寸）在装入后补发。
    pub(super) fn send_input(&self, surface: &str, input: &FrameInput) {
        self.post(PendingNotice::Input(surface.to_string(), input.clone()));
    }

    pub(super) fn send_tick(&self, surface: &str, frame: u64, delta_ms: u32) {
        let _ = self.with_plugin(|plugin| plugin.tick(surface, frame, delta_ms));
    }

    /// 转投一次服务调用；返回未送达的原因，调用方据此结束调用方的等待。
    pub(super) fn service_call(
        &self,
        call_id: u64,
        service: &str,
        method: &str,
        params: &str,
    ) -> CallState {
        self.with_plugin(|plugin| plugin.service_call(call_id, service, method, params))
    }

    /// 会话在跑且实例已装入（初始化窗口内为假）。
    pub(super) fn ready(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.alive && state.plugin.is_some()
    }

    fn with_plugin(&self, call: impl FnOnce(&PluginInstance)) -> CallState {
        let _entry = self.entry.lock();
        let plugin = {
            let state = self.state.lock().unwrap();
            if !state.alive {
                return CallState::Stopped;
            }
            match state.plugin.clone() {
                Some(plugin) => plugin,
                None => return CallState::Starting,
            }
        };
        call(&plugin);
        CallState::Delivered
    }

    /// 登记一次插件 → 宿主的调用；会话已收场时直接回可读失败。
    pub(super) fn register_call(&self, call: HostCall) -> bool {
        let request_id = call.request_id;
        let old = {
            let mut state = self.state.lock().unwrap();
            if !state.alive {
                drop(state);
                call.reply(false, "插件已停止，调用已取消");
                return false;
            }
            state.replies.insert(request_id, call)
        };
        if let Some(old) = old {
            old.reply(false, "请求 id 重复");
        }
        true
    }

    /// 回执一次调用；返回是否确有在途调用（迟到 / 已收场的回执返回 false）。
    pub(super) fn reply(&self, request_id: u64, ok: bool, payload: &str) -> bool {
        let call = {
            let mut state = self.state.lock().unwrap();
            if !state.alive {
                return false;
            }
            state.replies.remove(&request_id)
        };
        match call {
            Some(call) => {
                call.reply(ok, payload);
                true
            }
            None => false,
        }
    }

    pub(super) fn begin_service_call(&self, call_id: String) -> Option<u64> {
        let mut state = self.state.lock().unwrap();
        if !state.alive {
            return None;
        }
        let id = self.next_service_call.fetch_add(1, Ordering::Relaxed);
        state.service_calls.insert(id, call_id);
        Some(id)
    }

    pub(super) fn finish_service_call(&self, call_id: u64) -> Option<String> {
        self.state.lock().unwrap().service_calls.remove(&call_id)
    }

    /// 作废本次会话：拒绝新的入口调用与登记；已装入的实例留给随后的 [`AbiSession::shutdown`] 销毁。
    /// 用于销毁可能阻塞、但必须先让会话失效的路径（启动超时）。
    pub(super) fn invalidate(&self, token: u64) {
        let calls = {
            let mut state = self.state.lock().unwrap();
            if state.token != token {
                return;
            }
            state.alive = false;
            state.token = 0;
            state.pending.clear();
            state.service_calls.clear();
            take_calls(&mut state.replies)
        };
        fail_calls(calls, "插件会话已结束，调用已取消");
    }

    /// 结束会话：闸内摘实例与在途回执，闸外回执并销毁。
    ///
    /// 销毁会停插件的 worker / 泵线程，而这两条线程上的回调可能正等入口闸；持闸销毁会互等。
    pub(super) fn shutdown(&self) {
        let (plugin, calls) = {
            let _entry = self.entry.lock();
            let mut state = self.state.lock().unwrap();
            state.token = 0;
            state.alive = false;
            state.pending.clear();
            state.service_calls.clear();
            (state.plugin.take(), take_calls(&mut state.replies))
        };
        fail_calls(calls, "插件已停止，调用已取消");
        if let Some(plugin) = plugin {
            retire(plugin);
        }
    }
}

impl Drop for AbiSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 入口串行闸：同一时刻只让一个线程进入插件，并允许持闸线程在插件回调里再入。
///
/// 再入是必须的：插件可在 `on_attach` / `on_input` / `on_event` / `on_service_call` 里同步调用
/// 宿主函数，宿主处理会走回登记 / 回执路径；普通互斥锁会在同一线程上自锁。
#[derive(Default)]
struct EntryGate {
    inner: Mutex<GateState>,
    idle: Condvar,
}

#[derive(Default)]
struct GateState {
    owner: Option<std::thread::ThreadId>,
    depth: usize,
}

impl EntryGate {
    fn lock(&self) -> EntryGuard<'_> {
        let current = std::thread::current().id();
        let mut state = self.inner.lock().unwrap();
        while state.owner.is_some_and(|owner| owner != current) {
            state = self.idle.wait(state).unwrap();
        }
        state.owner = Some(current);
        state.depth += 1;
        EntryGuard { gate: self }
    }
}

struct EntryGuard<'a> {
    gate: &'a EntryGate,
}

impl Drop for EntryGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.inner.lock().unwrap();
        state.depth -= 1;
        if state.depth == 0 {
            state.owner = None;
            self.gate.idle.notify_one();
        }
    }
}

fn take_calls(replies: &mut HashMap<u64, HostCall>) -> Vec<HostCall> {
    replies.drain().map(|(_, call)| call).collect()
}

fn fail_calls(calls: Vec<HostCall>, reason: &str) {
    for call in calls {
        call.reply(false, reason);
    }
}

/// 入口闸已持有时销毁实例；排队中的调用已全部收场。
fn retire(plugin: Arc<PluginInstance>) {
    match Arc::try_unwrap(plugin) {
        Ok(plugin) => plugin.destroy(),
        Err(plugin) => drop(plugin),
    }
}

fn deliver(plugin: &PluginInstance, notice: PendingNotice) {
    match notice {
        PendingNotice::Attach(surface, kind) => plugin.attach(&surface, kind),
        PendingNotice::Detach(surface) => plugin.detach(&surface),
        PendingNotice::Event(name, payload) => plugin.event(&name, &payload),
        PendingNotice::Input(surface, input) => {
            let buffer = AbiInputBuf::new(&input);
            // SAFETY: 缓冲区及其字符串在本调用期间有效。
            unsafe { plugin.input(&surface, buffer.raw()) };
        }
    }
}

/// 一条输入连同它引用的字符串：指针指向本结构内的缓冲区，调用结束即失效。
struct AbiInputBuf {
    input: abi::AbiInput,
    text: String,
    hit: String,
}

impl AbiInputBuf {
    fn new(input: &FrameInput) -> Self {
        let mut buf = Self {
            input: abi::AbiInput {
                kind: 0,
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
                button: abi::BUTTON_LEFT,
                key: 0,
                text: abi::AbiStr::EMPTY,
                hit: abi::AbiStr::EMPTY,
                pressed: 0,
                modifiers: 0,
                delta_x: 0.0,
                delta_y: 0.0,
                composing: 0,
                has_caret: 0,
                caret: 0,
                candidates: abi::AbiRect::empty_at(0.0, 0.0),
                has_candidates: 0,
            },
            text: String::new(),
            hit: String::new(),
        };
        match input {
            FrameInput::PointerDown { x, y, button, hit } => {
                buf.input.kind = abi::INPUT_POINTER_DOWN;
                buf.input.pressed = 1;
                buf.set_point(*x, *y);
                buf.input.button = button_code(*button);
                buf.set_hit(hit.as_deref());
            }
            FrameInput::PointerUp { x, y, button, hit } => {
                buf.input.kind = abi::INPUT_POINTER_UP;
                buf.set_point(*x, *y);
                buf.input.button = button_code(*button);
                buf.set_hit(hit.as_deref());
            }
            FrameInput::PointerMove { x, y, hit } => {
                buf.input.kind = abi::INPUT_POINTER_MOVE;
                buf.set_point(*x, *y);
                buf.set_hit(hit.as_deref());
            }
            FrameInput::PointerLeave => buf.input.kind = abi::INPUT_POINTER_LEAVE,
            FrameInput::Outside { x, y } => {
                buf.input.kind = abi::INPUT_OUTSIDE;
                buf.set_point(*x, *y);
            }
            FrameInput::DoubleClick { x, y, button, hit } => {
                buf.input.kind = abi::INPUT_DOUBLE_CLICK;
                buf.input.pressed = 1;
                buf.set_point(*x, *y);
                buf.input.button = button_code(*button);
                buf.set_hit(hit.as_deref());
            }
            FrameInput::Wheel { x, y, delta_x, delta_y, modifiers, hit } => {
                buf.input.kind = abi::INPUT_WHEEL;
                buf.set_point(*x, *y);
                buf.input.delta_x = *delta_x;
                buf.input.delta_y = *delta_y;
                buf.input.modifiers = modifier_bits(*modifiers);
                buf.set_hit(hit.as_deref());
            }
            FrameInput::Key { key, action, modifiers, text } => {
                buf.input.kind = abi::INPUT_KEY;
                buf.input.pressed = (*action == KeyAction::Press) as u8;
                buf.input.modifiers = modifier_bits(*modifiers);
                match key {
                    Key::Named { name } => buf.input.key = named_key_code(*name),
                    Key::Character { text: character } => {
                        buf.input.key = abi::KEY_CHARACTER;
                        buf.set_text(text.as_deref().unwrap_or(character));
                    }
                }
                if let Key::Named { .. } = key {
                    if let Some(text) = text {
                        buf.set_text(text);
                    }
                }
            }
            FrameInput::Text { text, composing, caret, candidates } => {
                buf.input.kind = abi::INPUT_TEXT;
                buf.set_text(text);
                buf.input.composing = (*composing) as u8;
                if let Some(caret) = caret {
                    buf.input.has_caret = 1;
                    buf.input.caret = *caret;
                }
                if let Some(rect) = candidates {
                    buf.input.has_candidates = 1;
                    buf.input.candidates = abi::AbiRect::filled(
                        rect.x,
                        rect.y,
                        rect.w,
                        rect.h,
                        0.0,
                        abi::AbiColor::TRANSPARENT,
                    );
                }
            }
            FrameInput::Focus { focused } => {
                buf.input.kind = abi::INPUT_FOCUS;
                buf.input.pressed = (*focused) as u8;
            }
            FrameInput::Resize { width, height } => {
                buf.input.kind = abi::INPUT_RESIZE;
                buf.input.width = *width;
                buf.input.height = *height;
            }
        }
        buf
    }

    fn raw(&self) -> &abi::AbiInput {
        &self.input
    }

    fn set_point(&mut self, x: f32, y: f32) {
        self.input.x = x;
        self.input.y = y;
    }

    fn set_hit(&mut self, hit: Option<&str>) {
        self.hit.clear();
        if let Some(hit) = hit {
            self.hit.push_str(hit);
            self.input.hit = abi::AbiStr::new(&self.hit);
        }
    }

    fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.input.text = abi::AbiStr::new(&self.text);
    }
}

fn button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => abi::BUTTON_LEFT,
        MouseButton::Right => abi::BUTTON_RIGHT,
        MouseButton::Middle => abi::BUTTON_MIDDLE,
    }
}

fn modifier_bits(modifiers: Modifiers) -> u32 {
    let mut bits = 0;
    if modifiers.shift {
        bits |= abi::MOD_SHIFT;
    }
    if modifiers.control {
        bits |= abi::MOD_CONTROL;
    }
    if modifiers.alt {
        bits |= abi::MOD_ALT;
    }
    if modifiers.meta {
        bits |= abi::MOD_META;
    }
    bits
}

fn named_key_code(key: NamedKey) -> u32 {
    match key {
        NamedKey::Escape => abi::KEY_ESCAPE,
        NamedKey::Enter => abi::KEY_ENTER,
        NamedKey::Tab => abi::KEY_TAB,
        NamedKey::Space => abi::KEY_SPACE,
        NamedKey::Backspace => abi::KEY_BACKSPACE,
        NamedKey::Delete => abi::KEY_DELETE,
        NamedKey::Insert => abi::KEY_INSERT,
        NamedKey::Home => abi::KEY_HOME,
        NamedKey::End => abi::KEY_END,
        NamedKey::PageUp => abi::KEY_PAGE_UP,
        NamedKey::PageDown => abi::KEY_PAGE_DOWN,
        NamedKey::ArrowUp => abi::KEY_ARROW_UP,
        NamedKey::ArrowDown => abi::KEY_ARROW_DOWN,
        NamedKey::ArrowLeft => abi::KEY_ARROW_LEFT,
        NamedKey::ArrowRight => abi::KEY_ARROW_RIGHT,
        NamedKey::F1 => abi::KEY_F1,
        NamedKey::F2 => abi::KEY_F2,
        NamedKey::F3 => abi::KEY_F3,
        NamedKey::F4 => abi::KEY_F4,
        NamedKey::F5 => abi::KEY_F5,
        NamedKey::F6 => abi::KEY_F6,
        NamedKey::F7 => abi::KEY_F7,
        NamedKey::F8 => abi::KEY_F8,
        NamedKey::F9 => abi::KEY_F9,
        NamedKey::F10 => abi::KEY_F10,
        NamedKey::F11 => abi::KEY_F11,
        NamedKey::F12 => abi::KEY_F12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::frame::FrameRect;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    #[test]
    fn entry_gate_is_reentrant_on_the_same_thread() {
        let gate = EntryGate::default();
        let outer = gate.lock();
        drop(gate.lock());
        drop(outer);
        assert!(gate.inner.try_lock().is_ok(), "全部释放后闸应重新可用");
    }

    #[test]
    fn entry_gate_blocks_other_threads_until_released() {
        let gate = Arc::new(EntryGate::default());
        let held = gate.lock();
        let passed = Arc::new(AtomicBool::new(false));
        let worker = {
            let gate = gate.clone();
            let passed = passed.clone();
            std::thread::spawn(move || {
                drop(gate.lock());
                passed.store(true, Ordering::SeqCst);
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(!passed.load(Ordering::SeqCst), "持闸期间其他线程不得进入");
        drop(held);
        worker.join().unwrap();
        assert!(passed.load(Ordering::SeqCst));
    }

    #[test]
    fn input_buffer_projects_text_resize_and_hit() {
        let text = AbiInputBuf::new(&FrameInput::Text {
            text: "你好".into(),
            composing: true,
            caret: Some(2),
            candidates: Some(FrameRect { x: 1.0, y: 2.0, w: 30.0, h: 18.0 }),
        });
        let raw = text.raw();
        assert_eq!(raw.kind, abi::INPUT_TEXT);
        assert_eq!(raw.composing, 1);
        assert_eq!(raw.has_caret, 1);
        assert_eq!(raw.caret, 2);
        assert_eq!(raw.has_candidates, 1);
        assert_eq!(raw.candidates.w, 30.0);
        let copied = unsafe { std::slice::from_raw_parts(raw.text.data, raw.text.len) };
        assert_eq!(std::str::from_utf8(copied).unwrap(), "你好");

        let resize = AbiInputBuf::new(&FrameInput::Resize { width: 640.0, height: 480.0 });
        assert_eq!(resize.raw().kind, abi::INPUT_RESIZE);
        assert_eq!((resize.raw().width, resize.raw().height), (640.0, 480.0));

        let click = AbiInputBuf::new(&FrameInput::PointerDown {
            x: 3.0,
            y: 4.0,
            button: MouseButton::Right,
            hit: Some("next".into()),
        });
        let raw = click.raw();
        assert_eq!(raw.kind, abi::INPUT_POINTER_DOWN);
        assert_eq!(raw.button, abi::BUTTON_RIGHT);
        assert_eq!(raw.pressed, 1);
        let hit = unsafe { std::slice::from_raw_parts(raw.hit.data, raw.hit.len) };
        assert_eq!(std::str::from_utf8(hit).unwrap(), "next");
    }
}
