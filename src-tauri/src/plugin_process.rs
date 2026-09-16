//! 插件托管进程的宿主：启动（`ctx.shell.exec`/`spawn` 的后端）与「随应用退出收干净」。
//!
//! 清理归属必须落在**创建那一刻**：先让包装进程（`sh -c`/`cmd.exe /C`）跑起来再补登记，它可能
//! 已经派生出真正的服务进程，孙进程就漏在清理范围之外——那正是「应用关掉了，本机服务还在占端口
//! 与显存」的成因。所以进程由本模块创建，子进程在跑第一行代码之前就已属于清理范围。
//!
//! - **Windows**：`CREATE_SUSPENDED` 创建 → 加入设了 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的作业
//!   对象 → 恢复主线程。作业成员由子孙继承，且作业句柄随宿主进程消失被系统关闭时即触发全量结束，
//!   因此正常退出、崩溃、被任务管理器结束三条路径是同一条保证（`shutdown` 只把正常退出这条做得更
//!   显式）。
//! - **Unix**：子进程 `setpgid(0,0)` 自建进程组，宿主退出时按组 `SIGKILL`（组内一切进程，含包装层
//!   与全部子孙）。Unix 没有与作业对象等价的内核机制——`PR_SET_PDEATHSIG` 只作用于直接子进程，
//!   杀不掉 `sh -c` 派生出的服务——所以**崩溃/被强杀这一条路径不保证收干净**，只保证正常退出。
//!
//! 插件停用/卸载/更新仍只结束该插件的进程（前端按调用方记账后走 `kill_process_tree`），本模块只管
//! 应用级的两端：启动时的清理归属、退出时的收尾。
//!
//! 失败不静默：加入作业对象或恢复线程失败时**终止本次启动并返回原因**，而不是留一个宿主保证不了
//! 清理时机的进程在后面跑。

use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(unix)]
use std::collections::HashSet;
#[cfg(unix)]
use std::os::unix::process::CommandExt as UnixCommandExt;

/// 进程事件（与前端 `services/shell.ts` 的 `ProcessEvent` 逐字对齐）。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum ProcessEvent {
    Stdout { data: String },
    Stderr { data: String },
    /// 进程正常退出：`code` 为退出码，被信号终止时为 `None`。
    Terminated { code: Option<i32> },
    Error { message: String },
}

/// 事件出口：命令侧包一层 Tauri `Channel`，测试侧收进内存。
pub type EventSink = Arc<dyn Fn(ProcessEvent) + Send + Sync + 'static>;

/// 各平台放行的程序名（提示文案用；判定见 `resolve_program`）。
#[cfg(windows)]
const ALLOWED_PROGRAM: &str = "cmd.exe";
#[cfg(unix)]
const ALLOWED_PROGRAM: &str = "sh";
#[cfg(not(any(windows, unix)))]
const ALLOWED_PROGRAM: &str = "（本平台无可用解释器）";

/// 校验并解析要启动的程序。
///
/// 只放行 shell 解释器，**参数全开**——`ctx.shell` 是敏感面，其信任模型就是等价任意命令执行，
/// 这一层不是沙箱，只保证进程创建时能定下清理归属。
pub fn resolve_program(program: &str) -> Result<&'static str, String> {
    #[cfg(windows)]
    if program == "cmd.exe" {
        return Ok("cmd.exe");
    }
    #[cfg(unix)]
    if program == "sh" {
        // 固定用 /bin/sh：与 shell 解释器约定一致，不随 PATH 漂移
        return Ok("/bin/sh");
    }
    Err(format!("不允许启动程序「{program}」：插件进程只放行 {ALLOWED_PROGRAM}"))
}

/// 插件托管进程的宿主（应用内单例，经 `app.manage(Arc<PluginProcessHost>)` 托管）。
pub struct PluginProcessHost {
    #[cfg(windows)]
    job: Mutex<Option<JobHandle>>,
    /// 在册的直接子进程 pid（pid 即它自建的进程组 id）。仅 Unix 用：退出前按组结束。
    #[cfg(unix)]
    groups: Mutex<HashSet<u32>>,
    /// 已开始退出收尾：此后的启动一律拒绝，否则新进程会落在收尾之后、逃过清理。
    ///
    /// 正常退出路径上窗口已全部销毁、不会再有插件代码运行，此标志是防「收尾与新启动交叉」的
    /// 硬保证（不是靠时序巧合）。
    shutting_down: AtomicBool,
}

impl PluginProcessHost {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            job: Mutex::new(None),
            #[cfg(unix)]
            groups: Mutex::new(HashSet::new()),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// 启动进程：pid 到位即返回（不等进程结束），输出与退出经 `sink` 流式上报。
    ///
    /// 启动失败（程序不在白名单、工作目录不存在、无法纳入清理范围）返回可读原因。
    pub fn spawn(
        self: &Arc<Self>,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        env: Option<&HashMap<String, String>>,
        sink: EventSink,
    ) -> Result<u32, String> {
        let mut command = build_command(program, args, cwd, env)?;
        let child = self.create(&mut command)?;
        let pid = child.id();
        supervise(child, pid, sink, self.clone());
        Ok(pid)
    }

    /// 创建进程，并在它执行第一行代码之前把它纳入清理范围——并与退出收尾互斥。
    ///
    /// 两条路径都在「创建并纳入清理」与「开始收尾」之间做互斥，否则会出现「收尾已结束、新进程
    /// 才入册」的逃逸窗口（进程照常跑，清理却已收工）。互斥手段按平台取能覆盖住创建动作的那把锁：
    /// Windows 用作业对象槽位的锁（收尾也在同一把锁下置标志并结束作业），Unix 先入册再复查标志
    /// （进程组可从创建后任一时刻按组结束，复查即足够）。
    #[cfg(windows)]
    fn create(&self, command: &mut Command) -> Result<Child, String> {
        let mut slot = self.job.lock().unwrap();
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err("应用正在退出：不再启动插件进程".to_string());
        }
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let mut child = command.spawn().map_err(|e| format!("启动进程失败：{e}"))?;
        let pid = child.id();
        // 作业对象取不到就不必有这个进程：它还挂在挂起态、不在任何作业对象里，放走就是永久孤儿
        let job = match job_handle(&mut slot) {
            Ok(job) => job,
            Err(message) => {
                discard(&mut child);
                return Err(message);
            }
        };
        // 加入作业必须早于恢复执行：`cmd.exe` 一旦跑起来就可能已派生真正的服务，
        // 之后补加入作业会让孙进程留在作业之外。
        let assigned = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
        if assigned == 0 {
            let code = unsafe { GetLastError() };
            discard(&mut child);
            return Err(format!(
                "进程 {pid} 加入作业对象失败（错误码 {code}）：宿主无法保证它在应用退出时被结束，已终止本次启动"
            ));
        }
        if let Err(message) = resume_main_thread(pid) {
            discard(&mut child);
            return Err(message);
        }
        Ok(child)
    }

    #[cfg(unix)]
    fn create(&self, command: &mut Command) -> Result<Child, String> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err("应用正在退出：不再启动插件进程".to_string());
        }
        // pre_exec 里 setpgid(0,0)：子进程自建进程组（先于 exec 执行），退出收尾按组一次覆盖
        // 包装层与全部子孙。pid 即组 id。
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let mut child = command.spawn().map_err(|e| format!("启动进程失败：{e}"))?;
        let pid = child.id();
        self.groups.lock().unwrap().insert(pid);
        // 复查收尾标志：收尾若在入册前已完成，这个进程组不会被结束，必须在此补收（按组结束即
        // 覆盖它此后派生的一切），否则它就成了逃逸在清理之外的长驻服务。
        if self.shutting_down.load(Ordering::SeqCst) {
            unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
            self.untrack(pid);
            discard(&mut child);
            return Err("应用正在退出：已结束刚启动的插件进程".to_string());
        }
        Ok(child)
    }

    /// 其它平台没有可用的清理归属机制：宁可拒绝启动，也不放走一个无人回收的进程。
    #[cfg(not(any(windows, unix)))]
    fn create(&self, _command: &mut Command) -> Result<Child, String> {
        Err("当前平台不支持托管插件进程".to_string())
    }

    /// 进程退出后处置在册条目：只在**进程组已空**时摘除。
    ///
    /// 组 id 就是包装进程的 pid，但 `killpg` 杀的是组：包装层早退（`sh -c '服务 &'` 之类）而子孙
    /// 仍在跑时，摘掉条目等于把清理目标丢了——退出收尾再也收不到那个组，正是要消灭的残留形态。
    /// 组非空时 pgid 不会被内核回收，留着条目不会指向无关进程。
    ///
    /// 留下的代价：若子孙后来自行退出，条目会留到本次运行结束（组空后 pid 才可能被复用为别的组长，
    /// 那一刻起收尾的 `killpg` 会误伤无关组）。这是 Unix 没有作业对象可用的必然取舍——宁可留下
    /// 过期条目，不可漏掉仍在跑的服务。
    #[cfg(unix)]
    fn release(&self, pid: u32) {
        let empty = unsafe { libc::killpg(pid as i32, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if empty {
            self.groups.lock().unwrap().remove(&pid);
        }
    }

    #[cfg(not(unix))]
    fn release(&self, _pid: u32) {}

    /// 摘除在册进程（启动被拒绝时主动放弃；正常退出走 `release`）。
    #[cfg(unix)]
    fn untrack(&self, pid: u32) {
        self.groups.lock().unwrap().remove(&pid);
    }

    /// 结束全部在册插件托管进程（应用退出时调用一次）。
    ///
    /// 置收尾标志与结束动作必须同时成立（见 `create` 的互斥说明），故 Windows 侧在作业对象槽位
    /// 的同一把锁下完成，Unix 侧先置标志再按组结束、由 `create` 的复查兜住交错。
    pub fn shutdown_children(&self) {
        #[cfg(windows)]
        {
            let slot = self.job.lock().unwrap();
            self.shutting_down.store(true, Ordering::SeqCst);
            if let Some(job) = slot.as_ref() {
                // 显式结束整个作业：作业内一切进程（含 `cmd.exe` 派生出的服务）一并终止。
                // 句柄不在此关闭（作业对象由 self.job 持有到进程销毁）——崩溃路径下这一句不会执行，
                // 由系统关闭最后一个句柄时的 KILL_ON_JOB_CLOSE 兜底；两条路径不重合也无需重合。
                unsafe { TerminateJobObject(job.0, 0) };
            }
        }
        #[cfg(unix)]
        {
            self.shutting_down.store(true, Ordering::SeqCst);
            let pids: Vec<u32> = self.groups.lock().unwrap().iter().copied().collect();
            for pid in pids {
                // pid 即组 id（子进程 setpgid(0,0)）；ESRCH（已退出）不是问题，无需处理返回值
                unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
            }
        }
        #[cfg(not(any(windows, unix)))]
        self.shutting_down.store(true, Ordering::SeqCst);
    }
}

impl Default for PluginProcessHost {
    fn default() -> Self {
        Self::new()
    }
}

/// 取作业对象句柄，首次调用时创建并配上 `KILL_ON_JOB_CLOSE`。
///
/// 返回裸句柄：作业对象由槽位持有，调用方不得关闭它（关闭即触发全量结束）。调用方须已持有该槽位
/// 的锁——`create` 要靠这把锁与退出收尾互斥。
#[cfg(windows)]
fn job_handle(slot: &mut Option<JobHandle>) -> Result<HANDLE, String> {
    if let Some(job) = slot.as_ref() {
        return Ok(job.0);
    }
    let raw = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
    if raw.is_null() {
        return Err(format!("创建作业对象失败：{}", std::io::Error::last_os_error()));
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            &mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION as LPVOID,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as DWORD,
        )
    };
    if ok == 0 {
        let message = format!("配置作业对象失败：{}", std::io::Error::last_os_error());
        unsafe { CloseHandle(raw) };
        return Err(message);
    }
    *slot = Some(JobHandle(raw));
    Ok(raw)
}

/// 应用退出：结束全部插件托管进程。
///
/// 挂在 `RunEvent::Exit` 上——它是唯一的终态事件，此时窗口已全部销毁、不再有新的进程启动。
/// 崩溃与强杀不经过这里，由 Windows 作业对象随句柄关闭兜底（Unix 无等价机制，该路径留孤儿）。
pub fn shutdown(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(host) = app.try_state::<Arc<PluginProcessHost>>() else {
        return;
    };
    host.shutdown_children();
}

/// 组装命令：cwd 可选、`env` 为**追加/覆盖**宿主环境（不 `env_clear`——插件启动的本机服务需要
/// 宿主的 PATH/TEMP 等才有正常行为）。stdin 不接管：`ctx.shell` 没有写 stdin 的面。
fn build_command(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: Option<&HashMap<String, String>>,
) -> Result<Command, String> {
    let resolved = resolve_program(program)?;
    let mut command = Command::new(resolved);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = env {
        command.envs(env);
    }
    Ok(command)
}

/// 丢弃一个刚起、但没能纳入清理范围的进程（先收尸再返回，不留僵尸）。
fn discard(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// 进程守望：等进程结束 → 处置在册条目 → 等输出读完 → 上报退出。
///
/// 退出上报排在输出之后：聚合型调用（`ctx.shell.exec`）以退出事件为结果落地时刻，
/// 先报退出会把尾巴上的输出丢掉。若服务把 stdout 交给了长命子孙，读端不结束、退出上报会跟着
/// 延后（输出读完才算收尾）。
fn supervise(mut child: Child, pid: u32, sink: EventSink, host: Arc<PluginProcessHost>) {
    let out_reader = child
        .stdout
        .take()
        .map(|pipe| spawn_line_reader(pipe, sink.clone(), true));
    let err_reader = child
        .stderr
        .take()
        .map(|pipe| spawn_line_reader(pipe, sink.clone(), false));
    std::thread::spawn(move || {
        let status = child.wait();
        // 处置在册条目：包装进程结束不等于它这一组结束（见 release）
        host.release(pid);
        if let Some(reader) = out_reader {
            let _ = reader.join();
        }
        if let Some(reader) = err_reader {
            let _ = reader.join();
        }
        match status {
            Ok(status) => sink(ProcessEvent::Terminated { code: status.code() }),
            Err(e) => sink(ProcessEvent::Error { message: format!("等待进程 {pid} 结束失败：{e}") }),
        }
    });
}

/// 按行读一路输出并按行上报；读到结尾即退出。
fn spawn_line_reader(
    pipe: impl Read + Send + 'static,
    sink: EventSink,
    is_stdout: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(pipe);
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            match read_line(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(_) => match std::str::from_utf8(&buf) {
                    Ok(text) => {
                        let data = text.to_string();
                        sink(if is_stdout {
                            ProcessEvent::Stdout { data }
                        } else {
                            ProcessEvent::Stderr { data }
                        });
                    }
                    // 非 UTF-8 输出如实上报（不冒充某一路输出，也不静默丢弃）
                    Err(e) => sink(ProcessEvent::Error {
                        message: format!("进程输出解码失败：{e}"),
                    }),
                },
                Err(e) => {
                    sink(ProcessEvent::Error {
                        message: format!("读取进程输出失败：{e}"),
                    });
                    break;
                }
            }
        }
    })
}

/// 读一行：到 `\n` 或 `\r` 即算一行（终止符含在返回内容里，前端因此仍需按 `\r?\n` 自行切分）。
///
/// 用 tauri 自带的实现（tauri-plugin-shell 走的也是它），保证按行上报的切分语义与业界一致，
/// 不在这里另写一份等价副本。返回 0 = 输出结束。
fn read_line(reader: &mut impl BufRead, out: &mut Vec<u8>) -> std::io::Result<usize> {
    tauri::utils::io::read_line(reader, out)
}

// ---------- Windows：作业对象 ----------

#[cfg(windows)]
use winapi::shared::minwindef::{DWORD, FALSE, LPVOID};
#[cfg(windows)]
use winapi::shared::ntdef::HANDLE;
#[cfg(windows)]
use winapi::um::errhandlingapi::GetLastError;
#[cfg(windows)]
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use winapi::um::jobapi2::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
};
#[cfg(windows)]
use winapi::um::processthreadsapi::{OpenThread, ResumeThread};
#[cfg(windows)]
use winapi::um::tlhelp32::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
#[cfg(windows)]
use winapi::um::winnt::{
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, THREAD_SUSPEND_RESUME,
};

/// `CreateProcess` 创建后挂起主线程（在加入作业对象之前不让子进程执行任何代码）。
#[cfg(windows)]
const CREATE_SUSPENDED: DWORD = 0x0000_0004;
/// 不弹控制台窗口（插件起的是后台服务，没有交互界面）。
#[cfg(windows)]
const CREATE_NO_WINDOW: DWORD = 0x0800_0000;

/// 作业对象句柄。裸 HANDLE 不是 `Send`/`Sync`，但作业对象是进程级资源、句柄值可跨线程传递，
/// 这里显式声明（`Mutex` 已保证访问互斥）。
#[cfg(windows)]
struct JobHandle(HANDLE);

#[cfg(windows)]
unsafe impl Send for JobHandle {}
#[cfg(windows)]
unsafe impl Sync for JobHandle {}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        // 关闭最后一个句柄即触发 KILL_ON_JOB_CLOSE：这正是崩溃/强杀路径的兜底
        unsafe { CloseHandle(self.0) };
    }
}

/// 恢复被 `CREATE_SUSPENDED` 挂起的主线程。
///
/// 主线程句柄不暴露给 `std::process::Child`，只能按 pid 从线程快照里反查；刚创建的进程只有
/// 主线程一个线程（挂起状态下还跑不了任何代码）。找不到或恢复失败都必须上抛——子进程会一直
/// 挂起，留在后面只会变成一个永不工作的僵死条目。
#[cfg(windows)]
fn resume_main_thread(pid: u32) -> Result<(), String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "枚举线程失败（进程 {pid} 无法恢复执行）：{}",
            std::io::Error::last_os_error()
        ));
    }
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as DWORD;
    let mut main_thread: Option<u32> = None;
    let mut ok = unsafe { Thread32First(snapshot, &mut entry) };
    while ok != 0 {
        if entry.th32OwnerProcessID == pid {
            main_thread = Some(entry.th32ThreadID);
            break;
        }
        ok = unsafe { Thread32Next(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot) };
    let Some(thread_id) = main_thread else {
        return Err(format!("未找到进程 {pid} 的主线程：无法恢复执行"));
    };
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, FALSE, thread_id) };
    if thread.is_null() {
        return Err(format!(
            "打开进程 {pid} 的主线程失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let previous = unsafe { ResumeThread(thread) };
    unsafe { CloseHandle(thread) };
    if previous == u32::MAX {
        return Err(format!(
            "恢复进程 {pid} 主线程失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_shell_interpreters_are_allowed() {
        #[cfg(windows)]
        {
            assert!(resolve_program("cmd.exe").is_ok());
            for denied in ["sh", "python.exe", "powershell.exe", "cmd", ""] {
                assert!(resolve_program(denied).is_err(), "应拒绝：{denied}");
            }
        }
        #[cfg(unix)]
        {
            assert_eq!(resolve_program("sh").unwrap(), "/bin/sh");
            for denied in ["cmd.exe", "bash", "/bin/sh", "python3", ""] {
                assert!(resolve_program(denied).is_err(), "应拒绝：{denied}");
            }
        }
    }

    #[test]
    fn rejects_spawn_of_program_outside_whitelist() {
        let host = Arc::new(PluginProcessHost::new());
        let sink: EventSink = Arc::new(|_| {});
        let denied = if cfg!(windows) { "python.exe" } else { "bash" };
        let err = host
            .spawn(denied, &[], None, None, sink)
            .expect_err("白名单外的程序必须拒绝启动");
        assert!(err.contains(denied), "错误原因应指明被拒的程序：{err}");
    }
}
