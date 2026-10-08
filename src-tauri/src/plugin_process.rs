//! 插件托管进程的宿主：启动（`ctx.process.exec`/`spawn` 的后端）、stdin 写入与「随应用退出收干净」。
//! 清理归属必须在创建那一刻定下：先跑起来再补登记，`sh -c`/`cmd.exe /C` 包装进程可能已派生出
//! 真正的服务进程，孙进程会漏在清理范围外（「应用关掉了，本机服务还在占端口与显存」的成因）。
//! 失败不静默：纳入清理范围失败即终止本次启动并返回原因；插件停用/卸载按调用方记账走 `kill_process_tree`。

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
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

/// 校验并解析要启动的程序。
///
/// 程序来源**全部放行**（裸名交给 PATH 解析，路径形态按给定值使用）——这不是沙箱边界，
/// 信任模型本就等价任意命令执行，本层只负责两件事：空程序名在这里给出可读错误（而不是落到
/// OS 层报一堆码）、Unix 裸名 `sh` 固定解析到 `/bin/sh`（系统 shell 语义不随 PATH 漂移）。
pub fn resolve_program(program: &str) -> Result<String, String> {
    if program.trim().is_empty() {
        return Err("程序名不能为空".to_string());
    }
    #[cfg(unix)]
    if program == "sh" {
        return Ok("/bin/sh".to_string());
    }
    Ok(program.to_string())
}

/// 插件托管进程的宿主（应用内单例，经 `app.manage(Arc<PluginProcessHost>)` 托管）。
pub struct PluginProcessHost {
    #[cfg(windows)]
    job: Mutex<Option<JobHandle>>,
    /// 在册的直接子进程 pid（pid 即它自建的进程组 id）。仅 Unix 用：退出前按组结束。
    #[cfg(unix)]
    groups: Mutex<HashSet<u32>>,
    /// 在册进程的 stdin 写端（pid → 管道写端）。进程退出时由守望线程按**句柄身份**摘除（防 pid
    /// 复用误摘新进程的条目）；写路径只在取句柄时短暂持有此锁，阻塞 I/O 持的是条目自己的锁。
    stdins: Mutex<HashMap<u32, Arc<Mutex<ChildStdin>>>>,
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
            stdins: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// 启动进程：pid 到位即返回（不等进程结束），输出与退出经 `sink` 流式上报。
    ///
    /// 启动失败（工作目录不存在、无法纳入清理范围）返回可读原因。stdin 管道在启动时建立并
    /// 入册，供 `write_stdin` / `close_stdin` 使用。
    pub fn spawn(
        self: &Arc<Self>,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        env: Option<&HashMap<String, String>>,
        sink: EventSink,
    ) -> Result<u32, String> {
        let mut command = build_command(program, args, cwd, env)?;
        let mut child = self.create(&mut command)?;
        let pid = child.id();
        // 守望线程只持 Weak：强引用归注册表与写入路径——否则 close_stdin（仅从注册表摘除）后
        // 守望线程的强引用仍拖着写端，对端永远等不到 EOF
        let stdin_entry = child
            .stdin
            .take()
            .map(|stdin| Arc::new(Mutex::new(stdin)));
        if let Some(entry) = &stdin_entry {
            self.stdins.lock().unwrap().insert(pid, entry.clone());
        }
        supervise(
            child,
            pid,
            stdin_entry.as_ref().map(Arc::downgrade),
            sink,
            self.clone(),
        );
        Ok(pid)
    }

    /// 创建进程，并在它执行第一行代码之前把它纳入清理范围——并与退出收尾互斥。
    ///
    /// Windows 机制：`CREATE_SUSPENDED` 创建 → 加入设 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
    /// 的作业对象 → 恢复主线程。作业成员由子孙继承，作业句柄随宿主进程消失被系统关闭时即触发
    /// 全量结束——正常退出、崩溃、被任务管理器结束三条路径是同一条保证（`shutdown` 只把正常
    /// 退出这条做得更显式）。
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
    /// Unix 机制：子进程 `setpgid(0,0)` 自建进程组，宿主退出时按组 `SIGKILL`（组内一切进程，
    /// 含包装层与全部子孙）。Unix 没有与作业对象等价的内核机制，崩溃/被强杀路径不保证收干净。
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

    /// 向在册进程写入 stdin。
    ///
    /// 取句柄后立即释放注册表锁，阻塞 I/O 只持**条目自己的锁**——对端不读而管道写满时
    /// `write_all` 会无限期阻塞，绝不能让这层阻塞扩散到注册表（否则其它进程的启动/收尾全卡）。
    /// 同一进程的并发写按条目锁串行（保序）；对已退出进程写入按注册表缺席/写错误两条路径报可读错误。
    pub fn write_stdin(&self, pid: u32, data: &str) -> Result<(), String> {
        let entry = {
            let stdins = self.stdins.lock().unwrap();
            stdins.get(&pid).cloned()
        };
        let Some(entry) = entry else {
            return Err(format!("进程 {pid} 不存在或 stdin 已关闭，无法写入"));
        };
        let result = entry
            .lock()
            .unwrap()
            .write_all(data.as_bytes())
            .map_err(|e| format!("写入进程 {pid} 的 stdin 失败：{e}"));
        if result.is_err() {
            // 写失败（对端已关、管道断开）按句柄身份摘除，让后续写入落到「不存在」同一条错误路径
            self.remove_stdin_if(pid, &entry);
        }
        result
    }

    /// 关闭在册进程的 stdin（对端读到 EOF，长驻 helper 据此知道输入结束）。
    /// 进程已退出或已关闭时为 no-op。
    pub fn close_stdin(&self, pid: u32) {
        self.stdins.lock().unwrap().remove(&pid);
    }

    /// 仅当注册表里的条目仍是 `entry` 本尊时才摘除（守望线程退出清理与写入失败清理共用）。
    ///
    /// 按句柄身份而不是按 pid 盲摘：Unix 上 `wait` 收尸后 pid 立即可被复用，若新进程恰好拿到
    /// 同一 pid 并已入册，旧守望线程按 pid 摘除会砍掉**新进程**的写端。
    fn remove_stdin_if(&self, pid: u32, entry: &Arc<Mutex<ChildStdin>>) {
        let mut stdins = self.stdins.lock().unwrap();
        if stdins
            .get(&pid)
            .is_some_and(|cur| Arc::ptr_eq(cur, entry))
        {
            stdins.remove(&pid);
        }
    }

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
/// 宿主的 PATH/TEMP 等才有正常行为）。stdin 建管道：插件经 spawn 句柄写入（不传数据时写端
/// 随进程退出一并释放，对不读 stdin 的程序没有行为影响）。
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
        .stdin(Stdio::piped())
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
fn supervise(
    mut child: Child,
    pid: u32,
    stdin_entry: Option<std::sync::Weak<Mutex<ChildStdin>>>,
    sink: EventSink,
    host: Arc<PluginProcessHost>,
) {
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
        // 处置在册条目：包装进程结束不等于它这一组结束（见 release）；stdin 条目按**句柄身份**
        // 摘除（防 pid 复用后误摘新进程的写端），写端随之 drop、对端读到 EOF。条目已被
        // close_stdin 摘走时 Weak 升级失败，自然跳过
        host.release(pid);
        if let Some(entry) = stdin_entry.as_ref().and_then(std::sync::Weak::upgrade) {
            host.remove_stdin_if(pid, &entry);
        }
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

/// 测试共用替身（与 `commands::process` 的测试共享，避免逐字副本）。
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// 收集宿主上报的进程事件的测试替身。
    pub fn event_recorder() -> (Arc<std::sync::Mutex<Vec<ProcessEvent>>>, EventSink) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink: EventSink = {
            let seen = seen.clone();
            Arc::new(move |event| seen.lock().unwrap().push(event))
        };
        (seen, sink)
    }

    /// 等事件里出现退出事件，返回退出码（超时即失败）。
    pub fn wait_terminated(seen: &Arc<std::sync::Mutex<Vec<ProcessEvent>>>) -> Option<i32> {
        for _ in 0..50 {
            if let Some(ProcessEvent::Terminated { code }) = seen
                .lock()
                .unwrap()
                .iter()
                .find(|e| matches!(e, ProcessEvent::Terminated { .. }))
            {
                return *code;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::{event_recorder, wait_terminated};

    /// 本机可直接用的解释器（绝对路径形态：PATH 之外的程序按给定路径启动同样要能跑）。
    fn shell_program() -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
            (comspec, vec!["/C".to_string(), "echo hello".to_string()])
        }
        #[cfg(unix)]
        ("/bin/sh".to_string(), vec!["-c".to_string(), "echo hello".to_string()])
    }

    #[test]
    fn any_program_is_allowed() {
        // 信任模型 = 等价任意命令执行：裸名与路径形态一律放行，本层只挡空程序名
        for program in ["cmd.exe", "sh", "python", "powershell.exe", "bash", "/bin/sh", "C:\\tools\\ffmpeg.exe"] {
            assert!(resolve_program(program).is_ok(), "应放行：{program}");
        }
        #[cfg(unix)]
        assert_eq!(resolve_program("sh").unwrap(), "/bin/sh", "裸名 sh 固定解析到系统 shell");
        for denied in ["", "   "] {
            assert!(resolve_program(denied).is_err(), "应拒绝：{denied}");
        }
    }

    #[test]
    fn rejects_empty_program() {
        let host = Arc::new(PluginProcessHost::new());
        let sink: EventSink = Arc::new(|_| {});
        let err = host
            .spawn("", &[], None, None, sink)
            .expect_err("空程序名必须拒绝启动");
        assert!(err.contains("程序名"), "错误原因应指明程序名：{err}");
    }

    /// 绝对路径形态的程序要能直接启动（插件的侧车可执行不走 PATH）。
    #[test]
    fn spawns_program_by_absolute_path() {
        let host = Arc::new(PluginProcessHost::new());
        let (seen, sink) = event_recorder();
        let (program, args) = shell_program();
        let pid = host.spawn(&program, &args, None, None, sink).expect("按绝对路径启动失败");
        assert_ne!(pid, 0);
        assert_eq!(wait_terminated(&seen), Some(0), "应上报正常退出");
    }

    /// stdin 写入面：写进去的数据要到达子进程，close 后子进程收到 EOF 正常退出。
    #[test]
    fn stdin_round_trip() {
        let host = Arc::new(PluginProcessHost::new());
        let (seen, sink) = event_recorder();
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/C".to_string(), "more".to_string()]);
        #[cfg(unix)]
        let (program, args) = ("sh", vec!["-c".to_string(), "read line; echo \"got:$line\"".to_string()]);

        let pid = host.spawn(&program, &args, None, None, sink).expect("启动进程失败");
        host.write_stdin(pid, "hello\n").expect("写入 stdin 失败");
        host.close_stdin(pid);
        assert_eq!(wait_terminated(&seen), Some(0), "关闭 stdin 后子进程应正常退出");

        let stdout: String = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                ProcessEvent::Stdout { data } => Some(data.as_str()),
                _ => None,
            })
            .collect();
        assert!(stdout.contains("hello"), "stdin 数据应到达子进程输出：{stdout:?}");
    }

    /// 进程退出后 stdin 条目已摘除：写入必须报错（错误路径），而不是把数据写进悬空管道后静默。
    #[test]
    fn stdin_write_after_exit_fails() {
        let host = Arc::new(PluginProcessHost::new());
        let (seen, sink) = event_recorder();
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/C".to_string(), "echo x".to_string()]);
        #[cfg(unix)]
        let (program, args) = ("sh", vec!["-c".to_string(), "echo x".to_string()]);

        let pid = host.spawn(&program, &args, None, None, sink).expect("启动进程失败");
        assert_eq!(wait_terminated(&seen), Some(0), "进程应已退出");
        let err = host.write_stdin(pid, "late\n").expect_err("退出后写入 stdin 必须报错");
        assert!(err.contains(&pid.to_string()), "错误原因应指明 pid：{err}");
    }
}
