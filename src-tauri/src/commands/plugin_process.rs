//! 插件子进程运行时（Python）：spawn 解释器跑插件 main，经 stdio 逐行 JSON-RPC 与前端桥中转。
//!
//! 前端 `bridge.ts` 是全部插件的路由宿主（能力注册表/跨插件中转/审计），本模块只做
//! 「进程生命周期 + stdio 转发」，不承担任何业务路由：
//! - `plugin_process_start`：写 runner 临时文件 → spawn 解释器 → 起 stdout/stderr 读线程上报事件
//! - `plugin_process_write`：往子进程 stdin 写一行 JSON（异步 + 限长，不阻塞主线程）
//! - `plugin_process_kill`：终止进程并清理注册表条目
//!
//! 事件（前端经 `listen` 消费）：
//! - `plugin-process-message` `{ processId, message }`：子进程 stdout 的一行（已解析 JSON，解析失败为字符串）
//! - `plugin-process-stderr` `{ processId, line }`：stderr 行（调试/失败原因展示）
//! - `plugin-process-exit` `{ processId, code }`：stdout 关闭即视为进程退出（code 为 Option<i32>；
//!   注册表条目与 runner 临时文件随之清理）
//!
//! 临时文件时机：**spawn 后不删**——Python 在解释器初始化之后才打开脚本，spawn 即删会抢在打开之前
//! 导致启动失败（跨平台）；改为在进程退出（EOF 读线程）与 kill 路径清理。
//!
//! 编码与行：通信一律 UTF-8（spawn 设 `PYTHONUTF8=1` + runner 内 reconfigure 双保险）；
//! 读线程经 `fill_buf` 边读边限长、超限丢到换行、非 UTF-8 行跳过——防失控插件打爆内存且不中断通道。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::vault::VaultState;
use super::plugin::{read_manifest, resolve_plugin_dir, safe_plugin_path};

/** 单行（stdout/stderr/写入）长度上限：边读边限、超限丢到换行——防失控插件打爆内存；
 *  同时是协议消息的上限（超大载荷会被跳过/拒绝，属已知边界）。 */
const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub struct ProcessRegistry {
    inner: Arc<Mutex<HashMap<u64, ProcessEntry>>>,
}

struct ProcessEntry {
    plugin_dir: PathBuf,
    runner_path: PathBuf,
    child: Child,
    stdin: ChildStdin,
}

static NEXT_PROCESS_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Clone)]
struct ProcessMessage<'a> {
    process_id: u64,
    #[serde(rename = "message")]
    payload: &'a Value,
}

#[derive(Serialize, Clone)]
struct ProcessLine<'a> {
    process_id: u64,
    line: &'a str,
}

#[derive(Serialize, Clone)]
struct ProcessExit {
    process_id: u64,
    code: Option<i32>,
}

/// 读一行并限长：fill_buf/consume 边读边限，超限丢弃直到换行（不整行读入）；
/// 非 UTF-8 行返回空串（跳过不中断通道）。EOF/读错返回 None。
fn read_line_capped<R: BufRead>(reader: &mut R, cap: usize) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut overlong = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(a) if a.is_empty() => return None, // EOF
            Ok(a) => a,
            Err(_) => return None,
        };
        let nl = available.iter().position(|&b| b == b'\n');
        let take = nl.map_or(available.len(), |i| i + 1);
        if buf.len() + take > cap {
            overlong = true;
        } else if !overlong {
            buf.extend_from_slice(&available[..take]);
        }
        reader.consume(take);
        if nl.is_some() {
            if overlong || buf.is_empty() {
                return Some(String::new());
            }
            return match std::str::from_utf8(&buf) {
                Ok(s) => Some(s.trim_end().to_string()),
                Err(_) => Some(String::new()),
            };
        }
    }
}

/// 依次尝试候选解释器，返回首个 spawn 成功的 (child, stdin)。
fn spawn_runner(
    runtime: &str,
    runner_path: &Path,
    plugin_dir: &Path,
    main: &Path,
) -> Result<(Child, ChildStdin), String> {
    if runtime != "python" {
        return Err(format!("不支持的运行时：{runtime}"));
    }
    let candidates: &[&str] = &["python", "python3"];
    let mut last_err = format!("未找到 {runtime} 解释器（请安装并加入 PATH）");
    for cand in candidates {
        let mut cmd = Command::new(cand);
        cmd.arg(runner_path).arg(plugin_dir).arg(main);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // 与子进程的通信固定 UTF-8（默认简体中文 Windows 的 ACP=cp936 会输出 GBK 断通道）。
        if runtime == "python" {
            cmd.env("PYTHONUTF8", "1");
        }
        match cmd.spawn() {
            Ok(mut child) => {
                let stdin = child.stdin.take().ok_or("无法打开子进程 stdin")?;
                return Ok((child, stdin));
            }
            Err(e) => last_err = format!("{cand}: {e}"),
        }
    }
    Err(last_err)
}

fn runner_extension() -> &'static str {
    "py"
}

/// 启动插件子进程；返回 process_id（后续 write/kill 用它寻址）。
#[tauri::command]
pub async fn plugin_process_start(
    app: AppHandle,
    state: State<'_, VaultState>,
    registry: State<'_, ProcessRegistry>,
    id: String,
    runtime: String,
) -> Result<u64, String> {
    let (dir, _scope) = resolve_plugin_dir(&app, &state, &id)?;
    let manifest = read_manifest(&dir)?;
    // runtime 以清单为准：调用方传入不一致即拒绝（防御，正常路径恒等）。
    if manifest["runtime"].as_str().unwrap_or("js") != runtime {
        return Err(format!("运行时不一致：清单声明 {:?}，请求 {runtime}", manifest["runtime"]));
    }
    let main_rel = manifest["main"].as_str().ok_or("清单缺少 main")?;
    let main = safe_plugin_path(&dir, main_rel)?;

    let runner_src = include_str!("../../plugin_runtime/python_runner.py");
    // 临时文件名含进程内唯一 id + 时间戳：不可预测（防本地预置/junction 劫持），
    // 且重启后不与残留文件冲突。spawn 后不删——Python 初始化后才打开脚本，删早了启动失败。
    let process_id = NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed);
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let runner_path = std::env::temp_dir().join(format!(
        "atelyx-{id}-{runtime}-{process_id}-{unique:x}.{}",
        runner_extension()
    ));
    std::fs::write(&runner_path, runner_src).map_err(|e| format!("写入 runner 失败：{e}"))?;

    let (mut child, stdin) = spawn_runner(&runtime, &runner_path, &dir, &main)?;

    let stdout = child.stdout.take().ok_or("无法打开子进程 stdout")?;
    let stderr = child.stderr.take();

    // 同插件目录重复启动先清旧条目（重载场景；按目录匹配——app/vault 同 id 两作用域互不误杀）。
    {
        let mut guard = registry.inner.lock().unwrap();
        guard.retain(|_, e| {
            if e.plugin_dir == dir {
                let _ = std::fs::remove_file(&e.runner_path);
                let _ = e.child.kill();
                let _ = e.child.wait();
                false
            } else {
                true
            }
        });
        guard.insert(
            process_id,
            ProcessEntry { plugin_dir: dir, runner_path: runner_path.clone(), child, stdin },
        );
    }

    // stdout 读线程：每行 → plugin-process-message；EOF → 清条目 + 清 runner + 取退出码 + 发 exit。
    let app_out = app.clone();
    let app_exit = app.clone();
    let app_code = app.clone();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(line) = read_line_capped(&mut reader, MAX_LINE_BYTES) {
            if line.is_empty() {
                continue;
            }
            let parsed: Value = serde_json::from_str(&line).unwrap_or_else(|_| Value::String(line));
            let _ = app_out.emit("plugin-process-message", ProcessMessage { process_id, payload: &parsed });
        }
        // EOF：进程大概率已退出。清条目 + 清 runner；try_wait 取退出码，
        // 若进程仍存活（stdout 早关的罕见场景）显式 kill 防僵尸（条目已删，前端 kill 不可达）。
        // State 需先绑定：临时 State 在语句末即析构，guard 借其生命周期会 E0716。
        let registry_state = app_code.state::<ProcessRegistry>();
        let code = {
            let mut guard = registry_state.inner.lock().unwrap();
            guard
                .remove(&process_id)
                .map(|mut e| {
                    let _ = std::fs::remove_file(&e.runner_path);
                    match e.child.try_wait().ok().flatten() {
                        Some(status) => status.code(),
                        None => {
                            let _ = e.child.kill();
                            let _ = e.child.wait();
                            None
                        }
                    }
                })
                .flatten()
        };
        let _ = app_exit.emit("plugin-process-exit", ProcessExit { process_id, code });
    });
    // stderr 读线程：逐行转发（调试/失败原因；不触发退出事件）。
    if let Some(stderr) = stderr {
        let app_err = app.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            while let Some(line) = read_line_capped(&mut reader, MAX_LINE_BYTES) {
                if line.is_empty() {
                    continue;
                }
                let _ = app_err.emit("plugin-process-stderr", ProcessLine { process_id, line: &line });
            }
        });
    }

    Ok(process_id)
}

/// 往子进程 stdin 写一行 JSON（前端桥的 transport.post；异步 + spawn_blocking 不阻塞主线程）。
#[tauri::command]
pub async fn plugin_process_write(
    registry: State<'_, ProcessRegistry>,
    process_id: u64,
    line: String,
) -> Result<(), String> {
    if line.len() > MAX_LINE_BYTES {
        return Err("消息过大".into());
    }
    let inner = registry.inner.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = inner.lock().unwrap();
        let entry = guard.get_mut(&process_id).ok_or("进程不存在".to_string())?;
        entry
            .stdin
            .write_all(line.as_bytes())
            .map_err(|e| format!("写入子进程失败：{e}"))?;
        entry.stdin.write_all(b"\n").map_err(|e| format!("写入子进程失败：{e}"))?;
        entry.stdin.flush().map_err(|e| format!("写入子进程失败：{e}"))
    })
    .await
    .map_err(|e| format!("子进程写入任务失败：{e}"))?
}

/// 终止子进程并清理注册表条目与 runner 临时文件（前端 transport.dispose）。
#[tauri::command]
pub fn plugin_process_kill(registry: State<'_, ProcessRegistry>, process_id: u64) -> Result<(), String> {
    let mut guard = registry.inner.lock().unwrap();
    if let Some(mut entry) = guard.remove(&process_id) {
        let _ = std::fs::remove_file(&entry.runner_path);
        let _ = entry.child.kill();
        let _ = entry.child.wait();
    }
    Ok(())
}
