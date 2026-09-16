//! 插件托管进程的命令面：启动（`ctx.shell.exec`/`spawn` 的后端）与按 pid 结束进程树。
//!
//! 插件经 `ctx.shell.spawn`/`exec` 启动的长驻服务，包装层是 `sh -c`（Unix）/
//! `cmd.exe /C`（Windows）——只结束包装进程会把真正的服务留成孤儿（Windows 上尤其确定），
//! 所以这里按 pid 结束整棵树：Windows 用系统自带 `taskkill /T /F`，Linux 读 `/proc` 收齐
//! 子孙后从叶到根下发 SIGKILL（先结束父进程会让子孙改挂 init、再也枚举不到）。
//!
//! 启动侧（`spawn_plugin_process`）把进程创建收进宿主，好在创建那一刻就定下清理归属：
//! 作业对象/进程组与随应用退出的收尾见 `plugin_process.rs`。这一层不是沙箱——程序白名单只放行
//! shell 解释器而参数全开，与 `ctx.shell` 敏感面的信任模型一致。
//!
//! `kill_process_tree` 的信任模型与 `commands/external_fs.rs` 同款：命令接受任意 pid，不构成防插件
//! 边界——插件本可经 `ctx.shell.exec` 跑 `kill`/`taskkill`，原始命令逃生舱 `ctx.native.invoke` 亦全量
//! 放行。唯一的硬护栏是「不能杀掉整个应用/系统」：pid 0（Unix = 调用方进程组）与超出 i32 范围的
//! pid（`as i32` 会变成 -1 = 全部进程）在发起任何系统调用之前恒拒。

use std::collections::HashMap;
use std::sync::Arc;

use tauri::ipc::Channel;
use tauri::State;

use crate::plugin_process::{EventSink, PluginProcessHost, ProcessEvent};

// Windows 侧按 pid 结束走 taskkill 子进程
#[cfg(windows)]
use std::process::Command;
// Unix 侧进程树收集用（测试也直接调这两个纯函数，故 test 一并放行，避免 Windows 构建报 dead_code）
#[cfg(any(unix, test))]
use std::collections::HashSet;

/// 启动插件托管进程（`ctx.shell.exec`/`spawn` 的后端），pid 到位即返回。
///
/// 程序白名单由 `plugin_process` 校验，进程在创建时就纳入作业对象/进程组，输出与退出经
/// `on_event` 流式回传。
#[tauri::command(async)]
pub fn spawn_plugin_process(
    host: State<'_, Arc<PluginProcessHost>>,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    env: Option<HashMap<String, String>>,
    on_event: Channel<ProcessEvent>,
) -> Result<u32, String> {
    // 事件直通前端：发送失败（窗口已销毁）不重试，进程的清理不依赖事件送达
    let sink: EventSink = Arc::new(move |event| {
        let _ = on_event.send(event);
    });
    host.spawn(&program, &args, cwd.as_deref(), env.as_ref(), sink)
}

/// 结束 pid 及其全部子孙进程。
///
/// 进程已不存在不算失败（调用方可能只是重复停止）；其余失败返回可读原因，不静默。
/// 声明为 async：Windows 侧要起 `taskkill` 子进程（数十毫秒），同步命令会占住主线程
/// 事件循环，插件有多个在册进程时表现为停用瞬间界面卡顿。
#[tauri::command(async)]
pub fn kill_process_tree(pid: u32) -> Result<(), String> {
    // pid 0：Unix 的 kill(0) 命中调用方所在进程组、Windows 指向 Idle 进程——挡在系统调用之前
    if pid == 0 {
        return Err("拒绝结束 pid 0（会波及调用方进程组）".to_string());
    }
    #[cfg(unix)]
    {
        // pid 以 u32 进、以 i32 下发：超出 i32 范围会变成负数（-1 = 全部进程），必须先拒
        if pid > i32::MAX as u32 {
            return Err(format!("pid 超出可表示范围：{pid}"));
        }
        kill_tree_unix(pid)
    }
    #[cfg(windows)]
    {
        kill_tree_windows(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err("当前平台不支持结束进程树".to_string())
    }
}

/// ppid → 直接子进程集合。
#[cfg(any(unix, test))]
fn group_by_parent(parent_of: &HashMap<u32, u32>) -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&child, &parent) in parent_of {
        children.entry(parent).or_default().push(child);
    }
    children
}

/// 收齐 pid 的全部子孙（不含自身），发现序（先直接子、后更深层）。
///
/// `seen` 兜住环：`/proc` 正常不成环，但逐文件读取时拿到的是竞态快照（父子可能同时变动），
/// 不设防会死循环。
#[cfg(any(unix, test))]
fn descendants_of(pid: u32, children: &HashMap<u32, Vec<u32>>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(pid);
    let mut stack = vec![pid];
    while let Some(cur) = stack.pop() {
        let Some(kids) = children.get(&cur) else { continue };
        for &kid in kids {
            if seen.insert(kid) {
                out.push(kid);
                stack.push(kid);
            }
        }
    }
    out
}

/// 读 `/proc` 重建 ppid 关系（Linux）。
///
/// `/proc/<pid>/stat` 的 comm 字段可能含空格与括号（进程名可自定义），因此按**最后一个**
/// `)` 切开，其后的字段才是固定的 `state ppid ...`。读不到的条目跳过（进程刚好退出）。
#[cfg(target_os = "linux")]
fn process_parent_map() -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return map;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(idx) = stat.rfind(')') else { continue };
        let mut fields = stat[idx + 1..].split_whitespace();
        let _state = fields.next();
        let Some(ppid) = fields.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        map.insert(pid, ppid);
    }
    map
}

/// 非 Linux 的 unix（无 `/proc`）拿不到进程树：只能结束直接子进程。
#[cfg(all(unix, not(target_os = "linux")))]
fn process_parent_map() -> HashMap<u32, u32> {
    HashMap::new()
}

/// Unix 侧结束：收齐子孙后**从叶到根**下发 SIGKILL。
#[cfg(unix)]
fn kill_tree_unix(pid: u32) -> Result<(), String> {
    let mut targets = descendants_of(pid, &group_by_parent(&process_parent_map()));
    // 发现序是「父先子后」，倒过来即叶→根：父先死会让子孙改挂 init，后面就枚举不到了
    targets.reverse();
    targets.push(pid);

    let mut first_error: Option<String> = None;
    for target in targets {
        let rc = unsafe { libc::kill(target as i32, libc::SIGKILL) };
        if rc == 0 {
            continue;
        }
        let err = std::io::Error::last_os_error();
        // ESRCH = 进程已不存在（已在重启同 pid 前退出、或被子进程树结束顺带带走）不算失败
        if err.raw_os_error() != Some(libc::ESRCH) && first_error.is_none() {
            first_error = Some(format!("结束进程 {target} 失败：{err}"));
        }
    }
    match first_error {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// Windows 侧结束：`taskkill /T /F` 结束整棵树。
///
/// taskkill 的退出码与输出文案都不解析（会被本地化，且「进程已不存在」同样非零）；成败判据
/// 统一取「进程是否还在」（见 `windows_process_alive`）。但 taskkill **没跑起来**（找不到
/// 程序、进程创建失败）必须如实上抛——那与「进程仍在运行」是两种原因，混在一起无法定位。
#[cfg(windows)]
fn kill_tree_windows(pid: u32) -> Result<(), String> {
    // 参数数组直传，不经 shell 拼接
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .map_err(|e| format!("调用 taskkill 失败：{e}"))?;
    if windows_process_alive(pid)? {
        return Err(format!("结束进程 {pid} 失败（进程仍在运行）"));
    }
    Ok(())
}

/// 进程是否仍在运行（Windows）。
///
/// `OpenProcess` 失败即无句柄可用，按错误码区分：`ERROR_INVALID_PARAMETER` = pid 不存在，
/// 视为已结束；其余（权限不足等）保守当作仍存在，把失败如实上抛而不吞掉。
#[cfg(windows)]
fn windows_process_alive(pid: u32) -> Result<bool, String> {
    use winapi::shared::minwindef::FALSE;
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::minwinbase::STILL_ACTIVE;
    use winapi::um::processthreadsapi::{GetExitCodeProcess, OpenProcess};
    use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;

    /// `OpenProcess` 的「参数错误」：pid 不存在时返回此码。
    const ERROR_INVALID_PARAMETER: u32 = 87;

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if handle.is_null() {
        let code = unsafe { GetLastError() };
        return if code == ERROR_INVALID_PARAMETER {
            Ok(false)
        } else {
            Err(format!("查询进程 {pid} 失败（错误码 {code}）"))
        };
    }
    let mut exit_code: u32 = 0;
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return Err(format!("读取进程 {pid} 退出码失败"));
    }
    Ok(exit_code == STILL_ACTIVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn children_of(pairs: &[(u32, u32)]) -> HashMap<u32, Vec<u32>> {
        let parent_of: HashMap<u32, u32> = pairs.iter().map(|&(parent, child)| (child, parent)).collect();
        group_by_parent(&parent_of)
    }

    #[test]
    fn descendants_collect_whole_subtree() {
        // 10 → 20 → 30、10 → 21
        let children = children_of(&[(10, 20), (10, 21), (20, 30)]);
        let mut found = descendants_of(10, &children);
        found.sort_unstable();
        assert_eq!(found, vec![20, 21, 30]);
    }

    #[test]
    fn descendants_ignore_unrelated_trees() {
        let children = children_of(&[(10, 20), (99, 98)]);
        assert_eq!(descendants_of(10, &children), vec![20]);
        // 叶子节点无子孙
        assert!(descendants_of(20, &children).is_empty());
    }

    #[test]
    fn descendants_terminate_on_cycle() {
        // 竞态快照可能读出互相指认的父子（A→B 且 B→A）：必须终止而不是死循环
        let children = children_of(&[(10, 20), (20, 10)]);
        assert_eq!(descendants_of(10, &children), vec![20]);
    }

    #[test]
    fn rejects_pid_zero() {
        // pid 0 = 调用方进程组（Unix）；真的下发会杀掉测试进程自身
        let err = kill_process_tree(0).expect_err("pid 0 必须被拒");
        assert!(err.contains("pid 0"), "错误原因应指明 pid 0：{err}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_pid_beyond_i32() {
        // pid 以 u32 进、i32 下发：越界会变负数（-1 = 全部进程）
        let err = kill_process_tree(u32::MAX).expect_err("越界 pid 必须被拒");
        assert!(err.contains("超出可表示范围"), "错误原因应指明范围：{err}");
    }

    /// 起一个不依赖测试二进制、能活过测试时长的进程。
    fn spawn_long_running() -> std::process::Child {
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/C", "ping -n 31 127.0.0.1"]);
        #[cfg(unix)]
        let (program, args) = ("sh", vec!["-c", "sleep 30"]);
        std::process::Command::new(program)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("起测试进程失败")
    }

    /// 本次用例独占的孙进程 pid 文件路径。
    ///
    /// 用例并行执行，文件名必须逐个唯一：只用 pid + 毫秒会撞（同一毫秒内起两个用例就共用同一个
    /// 文件，一方清理会把另一方的 pid 文件删掉，表现为「未取到孙进程 pid」的偶发失败），故加自增序号。
    fn temp_pid_file() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "atelyx-tree-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// 起「包装进程 → 真正服务进程」两层结构用的启动参数：直接子进程是 `cmd.exe`/`sh` 包装，
    /// 长驻的是它下面的孙进程（把自己的 pid 写进 `pid_file`）。
    ///
    /// 复刻插件的真实形态（`cmd.exe /C <python> main.py`）：只断言包装进程消失不足以证明「进程树
    /// 结束」（去掉 `/T` 或 `/proc` 收集照样通过），所以让孙进程自报 pid，用例按该 pid 断言。
    fn wrapped_service_command(pid_file: &std::path::Path) -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            // 孙进程 = powershell（包装层是 cmd.exe）；写入自身 PID 后常驻。
            // 命令走**脚本文件**而不是 `-Command "…"`：内联命令要穿 cmd.exe 的引号与 `|` 解析，
            // 转义规则随调用方式变化（这是踩过的坑），落文件则命令行只有一个路径参数。
            let script_file = pid_file.with_extension("ps1");
            let script = format!(
                "$PID | Out-File -FilePath '{}' -Encoding ascii\nStart-Sleep -Seconds 60\n",
                pid_file.display()
            );
            std::fs::write(&script_file, script).expect("写测试脚本失败");
            let args = vec![
                "/C".to_string(),
                "powershell".to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-File".to_string(),
                script_file.to_string_lossy().into_owned(),
            ];
            ("cmd.exe".to_string(), args)
        }
        #[cfg(unix)]
        {
            let script = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
            ("sh".to_string(), vec!["-c".to_string(), script])
        }
    }

    /// 等孙进程写出自身 pid（Windows 上 powershell 启动尤其慢）。
    fn wait_service_pid(pid_file: &std::path::Path) -> u32 {
        let mut service = 0u32;
        for _ in 0..50 {
            if let Ok(text) = std::fs::read_to_string(pid_file) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    service = pid;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_ne!(service, 0, "未取到孙进程 pid（包装脚本未按预期写出自身 pid）");
        service
    }

    /// 清掉包装脚本与 pid 文件（Windows 的 .ps1 与 pid 文件同目录同主名）。
    fn cleanup_wrapped_files(pid_file: &std::path::Path) {
        let _ = std::fs::remove_file(pid_file);
        let _ = std::fs::remove_file(pid_file.with_extension("ps1"));
    }

    fn spawn_wrapped_service() -> (std::process::Child, u32) {
        let pid_file = temp_pid_file();
        let (program, args) = wrapped_service_command(&pid_file);
        let child = std::process::Command::new(program)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("起包装进程失败");
        let service = wait_service_pid(&pid_file);
        cleanup_wrapped_files(&pid_file);
        (child, service)
    }

    /// 收集宿主上报的进程事件的测试替身。
    fn event_recorder() -> (Arc<std::sync::Mutex<Vec<ProcessEvent>>>, EventSink) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink: EventSink = {
            let seen = seen.clone();
            Arc::new(move |event| seen.lock().unwrap().push(event))
        };
        (seen, sink)
    }

    /// 等事件里出现退出事件，返回退出码（超时即失败）。
    fn wait_terminated(seen: &Arc<std::sync::Mutex<Vec<ProcessEvent>>>) -> Option<i32> {
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

    /// 进程是否仍存在（未被回收的僵尸也算存在，故断言前必须先 wait 收尸）。
    #[cfg(unix)]
    fn is_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[cfg(windows)]
    fn is_alive(pid: u32) -> bool {
        windows_process_alive(pid).unwrap_or(true)
    }

    /// 轮询等待进程消失（结束是异步生效的，直接断言会偶发失败）。
    fn wait_gone(pid: u32) -> bool {
        for _ in 0..50 {
            if !is_alive(pid) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn kills_running_process() {
        let mut child = spawn_long_running();
        let pid = child.id();
        assert!(is_alive(pid), "测试进程应已启动：{pid}");

        kill_process_tree(pid).expect("结束进程树应成功");
        child.wait().expect("回收测试进程失败");
        assert!(wait_gone(pid), "进程 {pid} 应已被结束");
    }

    /// 核心语义：结束的是**整棵树**，不只是包装进程。
    ///
    /// 去掉 `/T`（Windows）或 `/proc` 子孙收集（Unix）时本用例必须失败——否则「插件停用后本机
    /// 服务仍在跑」这类回归无人守卫。等待带超时，避免结束生效的毫秒级延迟造成偶发失败。
    #[test]
    fn kills_wrapped_service_process_tree() {
        let (mut wrapper, service_pid) = spawn_wrapped_service();
        let wrapper_pid = wrapper.id();
        assert!(is_alive(service_pid), "真正服务进程应已启动：{service_pid}");

        kill_process_tree(wrapper_pid).expect("结束进程树应成功");
        wrapper.wait().expect("回收包装进程失败");

        let mut gone = false;
        for _ in 0..50 {
            if !is_alive(service_pid) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // 失败即清理：别把残留的服务进程留给后续用例/机器
        if !gone {
            kill_process_tree(service_pid).ok();
        }
        assert!(gone, "包装进程被结束，但真正服务进程仍在运行（只杀了包装层）");
    }

    #[test]
    fn killing_already_exited_process_is_ok() {
        // 进程退出并回收后：Unix 收 /proc 无果 → kill 得 ESRCH；Windows OpenProcess 得
        // ERROR_INVALID_PARAMETER。两条路径都必须判成功（重复停止不该报错）。
        let mut child = spawn_long_running();
        let pid = child.id();
        kill_process_tree(pid).expect("首次结束应成功");
        child.wait().expect("回收测试进程失败");
        assert!(wait_gone(pid), "进程 {pid} 应已被结束");

        kill_process_tree(pid).expect("重复结束已退出的进程不应报错");
    }

    #[cfg(windows)]
    #[test]
    fn windows_alive_check_distinguishes_self_and_bogus_pid() {
        assert!(windows_process_alive(std::process::id()).unwrap_or(false), "自身进程应判定为存活");
        // 高位的非法 pid：OpenProcess 返回 ERROR_INVALID_PARAMETER
        assert!(!windows_process_alive(0xFFFF_FFF0).unwrap_or(true), "非法 pid 应判定为不存在");
    }

    /// 宿主启动的进程要能上报输出与退出（`ctx.shell.exec` 的聚合结果建立在这条链上）。
    #[test]
    fn spawned_process_reports_output_and_exit() {
        let host = Arc::new(PluginProcessHost::new());
        let (seen, sink) = event_recorder();
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/C".to_string(), "echo hello".to_string()]);
        #[cfg(unix)]
        let (program, args) = ("sh", vec!["-c".to_string(), "echo hello".to_string()]);

        let pid = host.spawn(program, &args, None, None, sink).expect("启动进程失败");
        assert_ne!(pid, 0);
        assert_eq!(wait_terminated(&seen), Some(0), "应上报正常退出");

        let events = seen.lock().unwrap();
        let stdout: String = events
            .iter()
            .filter_map(|e| match e {
                ProcessEvent::Stdout { data } => Some(data.as_str()),
                _ => None,
            })
            .collect();
        assert!(stdout.contains("hello"), "输出应经回调上报：{stdout:?}（事件 {events:?}）");
    }

    /// 需求 A 的验收：应用退出时，插件托管进程**整棵树**都不再存在。
    ///
    /// 断言的是孙进程（真正的服务）——只结束包装进程的实现在这里必须失败。这是「应用关掉了，
    /// ComfyUI 还在占端口与显存」的直接守卫。
    #[test]
    fn shutdown_terminates_wrapped_service_tree() {
        let host = Arc::new(PluginProcessHost::new());
        let (_, sink) = event_recorder();
        let pid_file = temp_pid_file();
        let (program, args) = wrapped_service_command(&pid_file);

        let wrapper_pid = host
            .spawn(&program, &args, None, None, sink)
            .expect("启动包装进程失败");
        let service_pid = wait_service_pid(&pid_file);
        cleanup_wrapped_files(&pid_file);
        assert!(is_alive(service_pid), "真正服务进程应已启动：{service_pid}");

        host.shutdown_children();

        // 判据取清理前的实测值；失败即清理（别把残留服务留给后续用例/机器）不得覆盖判据，
        // 否则断言恒真、拿掉作业对象也照样通过
        let gone = wait_gone(service_pid) && wait_gone(wrapper_pid);
        if !gone {
            kill_process_tree(service_pid).ok();
            kill_process_tree(wrapper_pid).ok();
        }
        assert!(
            gone,
            "退出收尾后进程仍在运行（包装 {wrapper_pid} / 服务 {service_pid}）：清理没覆盖整棵树"
        );
    }

    /// 收尾之后不再接受新进程：否则新进程会落在清理之后、逃过退出收尾。
    #[test]
    fn spawn_is_refused_after_shutdown() {
        let host = Arc::new(PluginProcessHost::new());
        let (_, sink) = event_recorder();
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/C".to_string(), "echo x".to_string()]);
        #[cfg(unix)]
        let (program, args) = ("sh", vec!["-c".to_string(), "echo x".to_string()]);

        // 先起一个再收尾：收尾前可正常启动
        host.spawn(program, &args, None, None, sink.clone()).expect("收尾前应能启动");
        host.shutdown_children();

        let err = host
            .spawn(program, &args, None, None, sink)
            .expect_err("收尾后必须拒绝启动新进程");
        assert!(err.contains("退出"), "错误原因应指明退出中：{err}");
    }
}
