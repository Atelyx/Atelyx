//! 常驻运行时宿主：全应用单例的脚本运行时进程，承载插件宿主半模块（`ctx.rpc.attach` 的后端）。
//! 会话路由簿把运行时进程的每帧输出定向回发起会话的窗口；进程托管与退出收尾同 `ctx.process` 纪律。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::sync::mpsc;
use std::time::Duration;

use serde::Serialize;

use crate::plugin_process::{EventSink, PluginProcessHost, ProcessEvent};

/// 下行帧事件：载荷带会话 id 与 JSON-RPC 消息原文，前端按会话喂进对应通道。
#[cfg(not(test))]
pub const FRAME_EVENT: &str = "host-runtime-frame";
/// 会话结束事件：运行时侧主动结束（模块连续崩溃熔断等），前端据此关闭对应通道。
#[cfg(not(test))]
pub const SESSION_EVENT: &str = "host-runtime-session-ended";

/// attach 等待上限：模块加载（起线程 + import + activate）在时限内不到位即失败，不让命令悬挂。
const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);

/// attach 成功后返回的会话凭据（前端以其为键收发帧）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSession {
    pub session_id: u64,
    /// 承载会话的常驻运行时进程 pid（供诊断）。
    pub pid: u32,
}

/// 下行帧事件载荷。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FramePayload {
    pub session_id: u64,
    /// JSON-RPC 消息原文（单行 JSON，来自运行时进程的一帧输出）。
    pub frame: String,
}

/// 会话结束事件载荷。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndedPayload {
    pub session_id: u64,
    pub reason: String,
}

/// 广播出口的事件形状（生产 = Tauri 定向 emit 到窗口；测试 = 收集器）。
pub enum EmitterEvent {
    Frame { label: String, payload: FramePayload },
    Ended { label: String, payload: EndedPayload },
}

type EmitterFn = Box<dyn Fn(&EmitterEvent) + Send + Sync>;

fn poison<T>(err: PoisonError<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    err.into_inner()
}

/// 会话路由簿（纯表，可直测）：会话 id → 窗口 label + 在途 attach 的应答通道。
/// 会话 id 由宿主分配、单调递增不复用——复用会让迟到的旧帧投进新会话。
struct SessionBook {
    sessions: HashMap<u64, String>,
    pending: HashMap<u64, mpsc::Sender<Result<(), String>>>,
    next_session: u64,
}

impl SessionBook {
    fn new() -> Self {
        Self { sessions: HashMap::new(), pending: HashMap::new(), next_session: 0 }
    }

    /// 登记新会话并取在途应答通道（attach 请求发出前调用）。
    fn begin(&mut self, label: &str) -> (u64, mpsc::Receiver<Result<(), String>>) {
        self.next_session += 1;
        let session = self.next_session;
        self.sessions.insert(session, label.to_string());
        let (tx, rx) = mpsc::channel();
        self.pending.insert(session, tx);
        (session, rx)
    }

    /// attach 成功应答投递给等待方；会话不在途（迟到应答 / 路由已清）= false。
    fn attach_ok(&mut self, session: u64) -> bool {
        match self.pending.remove(&session) {
            Some(tx) => tx.send(Ok(())).is_ok(),
            None => false,
        }
    }

    /// attach 失败应答：会话从未就绪过，路由一并撤销。
    fn attach_err(&mut self, session: u64, error: &str) {
        self.sessions.remove(&session);
        if let Some(tx) = self.pending.remove(&session) {
            let _ = tx.send(Err(error.to_string()));
        }
    }

    /// 撤销会话（attach 超时 / detach / 会话结束），返回撤销前的归属窗口（在册才返回）。
    fn remove(&mut self, session: u64) -> Option<String> {
        self.pending.remove(&session);
        self.sessions.remove(&session)
    }

    fn lookup(&self, session: u64) -> Option<&str> {
        self.sessions.get(&session).map(String::as_str)
    }

    /// 摘除某窗口的全部会话，返回会话 id（窗口销毁 prune）。
    fn prune_label(&mut self, label: &str) -> Vec<u64> {
        let removed: Vec<u64> = self
            .sessions
            .iter()
            .filter(|(_, l)| l.as_str() == label)
            .map(|(s, _)| *s)
            .collect();
        for session in &removed {
            self.pending.remove(session);
            self.sessions.remove(session);
        }
        removed
    }

    /// 清空全部会话并让所有在途 attach 以失败落地（运行时进程死亡）。
    fn clear(&mut self, reason: &str) -> Vec<(u64, String)> {
        for (_, tx) in self.pending.drain() {
            let _ = tx.send(Err(reason.to_string()));
        }
        self.sessions.drain().collect()
    }
}

/// 常驻运行时宿主（应用内单例，经 `app.manage(Arc<HostRuntimeState>)` 托管）。
pub struct HostRuntimeState {
    inner: Mutex<Inner>,
    emitter: Mutex<Option<EmitterFn>>,
}

struct Inner {
    /// 运行时进程 pid（None = 尚未启动；首次会话惰性起，此后常驻不随空闲退出）。
    runtime: Option<u32>,
    book: SessionBook,
}

impl HostRuntimeState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner { runtime: None, book: SessionBook::new() }),
            emitter: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(poison)
    }

    /// 安装广播出口：帧与会话结束定向投递到发起会话的窗口。
    /// 出口由调用方注入——本 crate 不感知具体传输（壳侧接 Tauri 事件，测试接收集器）。
    pub fn install_emitter(&self, f: impl Fn(&EmitterEvent) + Send + Sync + 'static) {
        *self.emitter.lock().unwrap_or_else(poison) = Some(Box::new(f));
    }

    fn emit(&self, event: EmitterEvent) {
        let emitter = self.emitter.lock().unwrap_or_else(poison);
        if let Some(e) = emitter.as_ref() {
            e(&event);
        }
    }

    /// 建立会话：确保运行时进程在跑 → 登记路由 → 发 attach 控制 → 等待模块就绪。
    ///
    /// `node` / `script` 由命令层解析（资源目录随应用分发）；会话归属窗口 = 发起会话的窗口。
    /// 失败不留半建立状态：路由簿条目与运行时侧的在途加载都收干净。
    pub fn attach(
        self: &Arc<Self>,
        process_host: &Arc<PluginProcessHost>,
        window: &str,
        plugin_id: &str,
        module: &str,
        args: Option<serde_json::Value>,
        node: &str,
        script: &str,
    ) -> Result<HostSession, String> {
        if module.trim().is_empty() {
            return Err("宿主半模块路径不能为空".to_string());
        }
        if !std::path::Path::new(module).is_absolute() {
            return Err("宿主半模块路径必须是绝对路径".to_string());
        }
        if let Some(args) = &args {
            if !args.is_array() {
                return Err("宿主半模块参数（args）必须是数组".to_string());
            }
        }
        let (session, rx, pid, request) = {
            let mut inner = self.lock();
            let pid = match inner.runtime {
                Some(pid) => pid,
                None => self.spawn_locked(&mut inner, process_host, node, script)?,
            };
            let (session, rx) = inner.book.begin(window);
            let request = format!(
                "# {}\n",
                serde_json::json!({
                    "type": "attach", "session": session,
                    "pluginId": plugin_id, "module": module, "args": args,
                })
            );
            (session, rx, pid, request)
        };
        if let Err(message) = process_host.write_stdin(pid, &request) {
            // 写失败 = 管道已断（进程刚死）：按运行时死亡收场，下次 attach 会重新起进程
            self.runtime_died(format!("常驻运行时管道写入失败：{message}"));
            return Err(format!("常驻运行时会话建立失败：{message}"));
        }
        match rx.recv_timeout(ATTACH_TIMEOUT) {
            Ok(Ok(())) => Ok(HostSession { session_id: session, pid }),
            Ok(Err(message)) => Err(message),
            Err(_) => {
                let mut inner = self.lock();
                inner.book.remove(session);
                drop(inner);
                // 通知运行时侧收掉在途加载（模块加载挂死不残留线程）
                let _ = process_host.write_stdin(
                    pid,
                    &format!("# {}\n", serde_json::json!({ "type": "detach", "session": session })),
                );
                Err(format!(
                    "常驻运行时会话建立超时（{} 秒）：模块未在时限内就绪",
                    ATTACH_TIMEOUT.as_secs()
                ))
            }
        }
    }

    /// 启动运行时进程（必须持锁调用：事件由读线程异步到达，锁内落账才没有「死亡先于登记」的窗口）。
    fn spawn_locked(
        self: &Arc<Self>,
        inner: &mut Inner,
        process_host: &Arc<PluginProcessHost>,
        node: &str,
        script: &str,
    ) -> Result<u32, String> {
        let weak = Arc::downgrade(self);
        let args = vec![script.to_string()];
        let sink: EventSink = Arc::new(move |event| {
            let Some(state) = weak.upgrade() else { return };
            match event {
                ProcessEvent::Stdout { data } => state.handle_line(&data),
                // stderr 不参与协议（诊断材料）；decode/读故障按进程死亡收场——协议管道已不可信
                ProcessEvent::Stderr { .. } => {}
                ProcessEvent::Terminated { .. } => {
                    state.runtime_died("常驻运行时进程已退出".to_string());
                }
                ProcessEvent::Error { message } => {
                    state.runtime_died(format!("常驻运行时管道故障：{message}"));
                }
            }
        });
        let pid = process_host
            .spawn(node, &args, None, None, sink)
            .map_err(|message| format!("常驻运行时启动失败：{message}"))?;
        inner.runtime = Some(pid);
        Ok(pid)
    }

    /// 上行一帧：封前缀写入运行时进程 stdin。会话不在册 = 可读错误（前端通道据此收场）。
    pub fn send(&self, process_host: &Arc<PluginProcessHost>, session: u64, frame: &str) -> Result<(), String> {
        // 单行契约：内置换行会破坏前缀帧界（正常路径由 JSON.stringify 保证，防御性拒绝）
        let line = frame.trim_end_matches(['\n', '\r']);
        if line.is_empty() {
            return Err("会话帧不能为空".to_string());
        }
        if line.contains('\n') || line.contains('\r') {
            return Err("会话帧必须是单行".to_string());
        }
        let (pid, known) = {
            let inner = self.lock();
            (inner.runtime, inner.book.lookup(session).is_some())
        };
        if !known {
            return Err(format!("会话 {session} 不存在或已结束"));
        }
        let Some(pid) = pid else {
            return Err("常驻运行时未启动".to_string());
        };
        process_host.write_stdin(pid, &format!("{session} {line}\n"))
    }

    /// 卸载会话（停用/关闭通道）：路由摘除 + 通知运行时侧结束模块。幂等——会话不在册即成功。
    pub fn detach(&self, process_host: &Arc<PluginProcessHost>, session: u64) -> Result<(), String> {
        let (pid, existed) = {
            let mut inner = self.lock();
            (inner.runtime, inner.book.remove(session).is_some())
        };
        if !existed {
            return Ok(());
        }
        if let Some(pid) = pid {
            // 写失败（运行时已死）按成功处理：路由已摘除，「会话不再可达」本就是 detach 的目标
            let _ = process_host.write_stdin(
                pid,
                &format!("# {}\n", serde_json::json!({ "type": "detach", "session": session })),
            );
        }
        Ok(())
    }

    /// 摘除某窗口的全部会话并通知运行时侧结束对应模块（窗口销毁 prune，前端无从收尾）。
    pub fn prune_window(&self, process_host: &Arc<PluginProcessHost>, window: &str) {
        let (pid, sessions) = {
            let mut inner = self.lock();
            (inner.runtime, inner.book.prune_label(window))
        };
        if sessions.is_empty() {
            return;
        }
        if let Some(pid) = pid {
            for session in sessions {
                let _ = process_host.write_stdin(
                    pid,
                    &format!("# {}\n", serde_json::json!({ "type": "detach", "session": session })),
                );
            }
        }
    }

    /// 运行时进程的一帧输出：控制行（`# {json}`）或会话帧（`<session> <json>`）。
    fn handle_line(&self, raw: &str) {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            return;
        }
        if let Some(rest) = line.strip_prefix('#') {
            let Ok(control) = serde_json::from_str::<serde_json::Value>(rest) else {
                return; // 坏控制行跳过，不杀进程
            };
            let Some(session) = control.get("session").and_then(|v| v.as_u64()) else {
                return;
            };
            match control.get("type").and_then(|v| v.as_str()) {
                Some("attach-ok") => {
                    self.lock().book.attach_ok(session);
                }
                Some("attach-err") => {
                    let error = control
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("宿主半模块未就绪");
                    self.lock().book.attach_err(session, error);
                }
                Some("end") => {
                    let reason = control
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("宿主半模块会话已结束")
                        .to_string();
                    let label = self.lock().book.remove(session);
                    if let Some(label) = label {
                        self.emit(EmitterEvent::Ended {
                            label,
                            payload: EndedPayload { session_id: session, reason },
                        });
                    }
                }
                _ => {} // 未知控制类型跳过（前向兼容）
            }
            return;
        }
        let Some((id_text, frame)) = line.split_once(' ') else {
            return; // 无法解析的行跳过（协议外输出走 stderr）
        };
        let Ok(session) = id_text.parse::<u64>() else {
            return;
        };
        let label = {
            let inner = self.lock();
            inner.book.lookup(session).map(str::to_string)
        };
        if let Some(label) = label {
            self.emit(EmitterEvent::Frame {
                label,
                payload: FramePayload { session_id: session, frame: frame.to_string() },
            });
        }
        // 不在册会话的帧丢弃：窗口已销毁 / 会话已结束的迟到输出
    }

    /// 运行时进程死亡：清空进程与全部会话路由，在途 attach 与活会话都以可读原因落地。
    fn runtime_died(&self, reason: String) {
        let ended = {
            let mut inner = self.lock();
            inner.runtime = None;
            inner.book.clear(&reason)
        };
        for (session, label) in ended {
            self.emit(EmitterEvent::Ended {
                label,
                payload: EndedPayload { session_id: session, reason: reason.clone() },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// 收集定向 emit 的测试替身：按事件种类分桶。
    struct Collector {
        frames: Mutex<Vec<(String, FramePayload)>>,
        ends: Mutex<Vec<(String, EndedPayload)>>,
    }

    impl Collector {
        fn new() -> Arc<Self> {
            Arc::new(Self { frames: Mutex::new(Vec::new()), ends: Mutex::new(Vec::new()) })
        }
    }

    fn install(state: &HostRuntimeState, collector: &Arc<Collector>) {
        let c = collector.clone();
        state.install_emitter(move |ev| match ev {
            EmitterEvent::Frame { label, payload } => {
                c.frames.lock().unwrap().push((label.clone(), payload.clone()));
            }
            EmitterEvent::Ended { label, payload } => {
                c.ends.lock().unwrap().push((label.clone(), payload.clone()));
            }
        });
    }

    // ---------- SessionBook 纯表 ----------

    #[test]
    fn book_assign_and_lookup() {
        let mut book = SessionBook::new();
        let (a, _rx) = book.begin("main");
        let (b, _rx) = book.begin("panel-1");
        assert_ne!(a, b, "会话 id 必须不复用");
        assert_eq!(book.lookup(a), Some("main"));
        assert_eq!(book.lookup(b), Some("panel-1"));
        assert_eq!(book.lookup(b + 1), None);
    }

    #[test]
    fn book_attach_settlement() {
        let mut book = SessionBook::new();
        let (session, rx) = book.begin("main");
        book.attach_ok(session);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(Ok(())));
        // 已结算的会话重复应答（迟到旧帧）不再投递
        book.attach_ok(session);
        assert_eq!(book.lookup(session), Some("main"), "成功应答保留路由");
    }

    #[test]
    fn book_attach_err_removes_session() {
        let mut book = SessionBook::new();
        let (session, rx) = book.begin("main");
        book.attach_err(session, "模块加载失败");
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(Err("模块加载失败".into())));
        assert_eq!(book.lookup(session), None, "失败会话的路由必须撤销");
    }

    #[test]
    fn book_clear_fails_pending_and_returns_sessions() {
        let mut book = SessionBook::new();
        let (attached, _rx) = book.begin("main");
        let (attaching, rx) = book.begin("panel-1");
        let ended = book.clear("运行时已退出");
        assert!(ended.contains(&(attached, "main".to_string())));
        assert!(ended.contains(&(attaching, "panel-1".to_string())));
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(Err("运行时已退出".into())));
        assert_eq!(book.lookup(attached), None);
    }

    #[test]
    fn book_prune_only_target_window() {
        let mut book = SessionBook::new();
        let (a, _rx) = book.begin("main");
        let (b, _rx) = book.begin("panel-1");
        let (c, _rx) = book.begin("panel-1");
        let mut pruned = book.prune_label("panel-1");
        pruned.sort_unstable();
        assert_eq!(pruned, vec![b, c]);
        assert_eq!(book.lookup(a), Some("main"));
    }

    // ---------- handle_line：控制行与会话帧 ----------

    #[test]
    fn control_lines_route_by_type() {
        let state = Arc::new(HostRuntimeState::new());
        let collector = Collector::new();
        install(&state, &collector);

        let (session, rx) = {
            let mut inner = state.lock();
            let (s, rx) = inner.book.begin("main");
            (s, rx)
        };

        state.handle_line(&format!("# {}\n", serde_json::json!({ "type": "attach-ok", "session": session })));
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(Ok(())));

        // attach-err 撤路由并投递原因
        let (session2, rx2) = {
            let mut inner = state.lock();
            let (s, rx) = inner.book.begin("main");
            (s, rx)
        };
        state.handle_line(&format!(
            "# {}\n",
            serde_json::json!({ "type": "attach-err", "session": session2, "error": "导入失败" })
        ));
        assert_eq!(rx2.recv_timeout(Duration::from_secs(1)), Ok(Err("导入失败".into())));
        {
            let inner = state.lock();
            assert_eq!(inner.book.lookup(session2), None);
        }

        // end：撤路由 + 定向会话结束事件
        state.handle_line(&format!(
            "# {}\n",
            serde_json::json!({ "type": "end", "session": session, "reason": "连续崩溃熔断" })
        ));
        {
            let inner = state.lock();
            assert_eq!(inner.book.lookup(session), None);
        }
        let ends = collector.ends.lock().unwrap();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].0, "main");
        assert_eq!(ends[0].1.session_id, session);
        assert_eq!(ends[0].1.reason, "连续崩溃熔断");
    }

    #[test]
    fn session_frames_route_to_window_and_unknown_sessions_drop() {
        let state = Arc::new(HostRuntimeState::new());
        let collector = Collector::new();
        install(&state, &collector);
        let session = {
            let mut inner = state.lock();
            let (s, _rx) = inner.book.begin("panel-1");
            s
        };
        state.handle_line(&format!("{session} {{\"jsonrpc\":\"2.0\"}}\n"));
        {
            let frames = collector.frames.lock().unwrap();
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].0, "panel-1");
            assert_eq!(frames[0].1.session_id, session);
            assert_eq!(frames[0].1.frame, "{\"jsonrpc\":\"2.0\"}");
        }
        // 不在册会话：静默丢弃（窗口已销毁 / 会话已结束的迟到输出）
        state.handle_line(&format!("999999 {{\"jsonrpc\":\"2.0\"}}\n"));
        assert!(collector.frames.lock().unwrap().len() == 1);
        // 坏行（无空格 / 非数字 id）跳过不杀进程
        state.handle_line("garbage\n");
        state.handle_line("x y\n");
        assert!(collector.frames.lock().unwrap().len() == 1);
    }

    // ---------- 端到端：真进程 + 前缀帧协议 ----------

    /// 从 PATH 找 node（cargo test 运行环境必有 Node：pnpm 工具链依赖）。
    fn node_program() -> Option<String> {
        #[cfg(windows)]
        {
            for name in ["node.exe", "node"] {
                if let Ok(output) = std::process::Command::new("where").arg(name).output() {
                    if output.status.success() {
                        let first = String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if !first.is_empty() {
                            return Some(first);
                        }
                    }
                }
            }
            None
        }
        #[cfg(not(windows))]
        {
            std::process::Command::new("sh")
                .args(["-c", "command -v node"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        }
    }

    /// 最小 supervisor 替身：attach 即应 ok，会话帧原样回显（校验前缀帧解析与路由闭环）。
    fn stub_supervisor(dir: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("stub-supervisor.mjs");
        std::fs::write(
            &script,
            r##"
import { createInterface } from "node:readline";
import { stdin, stdout } from "node:process";
createInterface({ input: stdin }).on("line", (line) => {
  if (!line.trim()) return;
  if (line.startsWith("#")) {
    const c = JSON.parse(line.slice(1));
    if (c.type === "attach") stdout.write(`# ${JSON.stringify({ type: "attach-ok", session: c.session })}\n`);
    else if (c.type === "detach") stdout.write(`# ${JSON.stringify({ type: "end", session: c.session, reason: "detached" })}\n`);
    return;
  }
  const sp = line.indexOf(" ");
  stdout.write(`${line.slice(0, sp)} ${JSON.stringify({ echo: JSON.parse(line.slice(sp + 1)) })}\n`);
});
"##,
        )
        .expect("替身 supervisor 写入失败");
        script
    }

    #[test]
    fn end_to_end_attach_send_detach() {
        let Some(node) = node_program() else {
            eprintln!("skipping: PATH 上没有 node");
            return;
        };
        let state = Arc::new(HostRuntimeState::new());
        let collector = Collector::new();
        install(&state, &collector);
        let process_host = Arc::new(PluginProcessHost::new());
        let dir = std::env::temp_dir().join("atelyx-host-runtime-test");
        std::fs::create_dir_all(&dir).expect("临时目录创建失败");
        let script = stub_supervisor(&dir);

        let started = state
            .attach(&process_host, "main", "com.test.plugin", &script.to_string_lossy(), None, &node, &script.to_string_lossy())
            .expect("attach 失败");
        assert_eq!(started.session_id, 1);
        assert_ne!(started.pid, 0);

        // 会话帧回显经路由簿定向到发起窗口
        state
            .send(&process_host, started.session_id, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
            .expect("send 失败");
        let mut frames = VecDeque::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            frames = collector.frames.lock().unwrap().drain(..).collect();
            if !frames.is_empty() {
                break;
            }
        }
        assert_eq!(frames.len(), 1, "回显帧应经事件投递");
        assert_eq!(frames[0].0, "main");
        assert_eq!(frames[0].1.session_id, started.session_id);
        let echoed: serde_json::Value = serde_json::from_str(&frames[0].1.frame).expect("回显帧应为合法 JSON");
        assert_eq!(echoed["echo"]["method"], "ping");

        // detach：路由先摘除，替身回的 end 是迟到帧、正确丢弃（end 控制行为运行时侧主动结束设计，
        // 已由 control_lines_route_by_type 覆盖）；这里只验证幂等与本侧收场
        state.detach(&process_host, started.session_id).expect("detach 失败");
        {
            let inner = state.lock();
            assert_eq!(inner.book.lookup(started.session_id), None, "detach 后路由必须摘除");
        }
        assert!(
            collector.ends.lock().unwrap().is_empty(),
            "本侧主动 detach 不产生会话结束事件（通道由 close 语义收场）"
        );

        // 幂等：再 detach 报成功
        assert!(state.detach(&process_host, started.session_id).is_ok());

        // 收尾：结束运行时进程（替身常驻，不结束会拖到测试进程退出）
        process_host.shutdown_children();
    }

    #[test]
    fn attach_rejects_bad_module() {
        let state = Arc::new(HostRuntimeState::new());
        let process_host = Arc::new(PluginProcessHost::new());
        let err = state
            .attach(&process_host, "main", "com.test", "host.mjs", None, "node", "supervisor.mjs")
            .expect_err("相对路径必须拒绝");
        assert!(err.contains("绝对路径"), "错误应指明路径要求：{err}");
        let err = state
            .attach(&process_host, "main", "com.test", "C:\\x\\host.mjs", Some(serde_json::json!("not-array")), "node", "s.mjs")
            .expect_err("非数组 args 必须拒绝");
        assert!(err.contains("数组"), "错误应指明 args 要求：{err}");
    }

    /// 运行时进程死亡：在途 attach 以失败落地、活会话收到结束事件、下次 attach 重新起进程。
    #[test]
    fn runtime_death_fails_pending_and_ends_sessions() {
        let Some(node) = node_program() else {
            eprintln!("skipping: PATH 上没有 node");
            return;
        };
        let state = Arc::new(HostRuntimeState::new());
        let collector = Collector::new();
        install(&state, &collector);
        let process_host = Arc::new(PluginProcessHost::new());
        let dir = std::env::temp_dir().join("atelyx-host-runtime-test");
        let script = stub_supervisor(&dir);

        let started = state
            .attach(&process_host, "main", "com.test.plugin", &script.to_string_lossy(), None, &node, &script.to_string_lossy())
            .expect("attach 失败");

        // 直接杀进程：死亡事件异步到达后清空路由
        let _ = process_host.shutdown_children();
        std::thread::sleep(Duration::from_millis(500));
        {
            let inner = state.lock();
            assert!(inner.runtime.is_none(), "死亡事件应清空运行时 pid");
            assert_eq!(inner.book.lookup(started.session_id), None, "活会话应随死亡清空");
        }
        let ends = collector.ends.lock().unwrap();
        assert!(ends.iter().any(|(label, p)| label == "main" && p.session_id == started.session_id));
    }

    /// 真 supervisor + 真 worker 线程 + 真模块：握手、方法调用与响应的完整闭环。
    #[test]
    fn real_supervisor_end_to_end_call() {
        let Some(node) = node_program() else {
            eprintln!("skipping: PATH 上没有 node");
            return;
        };
        let supervisor = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../legacy/src-tauri/resources/host-runtime.mjs");
        assert!(supervisor.is_file(), "supervisor 脚本应随源码入库");

        // 插件宿主半模块：activate 返回描述符（ping 方法 + serverInfo）
        let dir = std::env::temp_dir().join("atelyx-host-runtime-test");
        std::fs::create_dir_all(&dir).expect("临时目录创建失败");
        let module = dir.join("e2e-host.mjs");
        std::fs::write(
            &module,
            "export default function activate(host, args) {\n  return {\n    serverInfo: { name: \"e2e\", version: \"0.1.0\" },\n    methods: { ping: (p) => ({ pong: p.v, args }) },\n  };\n}\n",
        )
        .expect("宿主半模块写入失败");

        let state = Arc::new(HostRuntimeState::new());
        let collector = Collector::new();
        install(&state, &collector);
        let process_host = Arc::new(PluginProcessHost::new());

        let started = state
            .attach(&process_host, "main", "com.test.plugin", &module.to_string_lossy(), Some(serde_json::json!(["cfg"])), &node, &supervisor.to_string_lossy())
            .expect("真 supervisor attach 失败");

        // initialize 握手：supervisor 按描述符应答
        state
            .send(&process_host, started.session_id, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"client":{}}}"#)
            .expect("握手帧发送失败");
        let handshake = wait_frame(&collector, started.session_id, |m| m.get("id") == Some(&serde_json::json!(1)));
        assert_eq!(handshake["result"]["protocolVersion"], 1);
        assert_eq!(handshake["result"]["serverInfo"]["name"], "e2e");

        // 方法调用：模块线程处理并返回结果（args 透传可见）
        state
            .send(&process_host, started.session_id, r#"{"jsonrpc":"2.0","id":2,"method":"ping","params":{"v":42}}"#)
            .expect("调用帧发送失败");
        let reply = wait_frame(&collector, started.session_id, |m| m.get("id") == Some(&serde_json::json!(2)));
        assert_eq!(reply["result"]["pong"], 42);
        assert_eq!(reply["result"]["args"], serde_json::json!(["cfg"]));

        process_host.shutdown_children();
    }

    /// 真 supervisor + 不存在的模块：attach-err 以可读原因失败，会话无残留（可立即重试 attach）。
    #[test]
    fn real_supervisor_bad_module_fails_cleanly() {
        let Some(node) = node_program() else {
            eprintln!("skipping: PATH 上没有 node");
            return;
        };
        let supervisor = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../legacy/src-tauri/resources/host-runtime.mjs");
        let state = Arc::new(HostRuntimeState::new());
        let collector = Collector::new();
        install(&state, &collector);
        let process_host = Arc::new(PluginProcessHost::new());

        let missing = std::env::temp_dir().join("atelyx-host-runtime-test/no-such-module.mjs");
        let error = state
            .attach(&process_host, "main", "com.test.plugin", &missing.to_string_lossy(), None, &node, &supervisor.to_string_lossy())
            .expect_err("不存在的模块必须失败");
        assert!(!error.is_empty(), "失败原因应可读：{error}");
        {
            let inner = state.lock();
            assert_eq!(inner.book.lookup(1), None, "失败会话的路由应已撤销");
            assert!(inner.runtime.is_some(), "运行时进程应保持存活供后续 attach 复用");
        }
        // 同一运行时进程上重试成功的模块：失败不留下后遗症
        let dir = std::env::temp_dir().join("atelyx-host-runtime-test");
        std::fs::create_dir_all(&dir).expect("临时目录创建失败");
        let module = dir.join("retry-host.mjs");
        std::fs::write(&module, "export default () => ({ methods: {} });\n").expect("模块写入失败");
        let retried = state
            .attach(&process_host, "main", "com.test.plugin", &module.to_string_lossy(), None, &node, &supervisor.to_string_lossy())
            .expect("失败后重试 attach 应成功");
        assert_ne!(retried.session_id, 1, "会话 id 单调不复用");
        process_host.shutdown_children();
    }

    /// 等指定会话的下一帧（谓词匹配）；超时 panic。
    fn wait_frame(
        collector: &Arc<Collector>,
        session_id: u64,
        matches: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            let mut frames = collector.frames.lock().unwrap();
            if let Some(at) = frames
                .iter()
                .position(|(label, p)| label == "main" && p.session_id == session_id && {
                    serde_json::from_str::<serde_json::Value>(&p.frame)
                        .map(|m| matches(&m))
                        .unwrap_or(false)
                })
            {
                let (_, payload) = frames.remove(at);
                drop(frames);
                return serde_json::from_str(&payload.frame).expect("帧应为合法 JSON");
            }
        }
        panic!("等待会话帧超时（session {session_id}）");
    }
}
