//! 打开文件上下文宿主（跨窗口协调态的单进程真源）：主窗口唯一写者，撕裂窗口经 boot 拉取
//! 基线 + 订阅广播跟随，窗口间不再互相广播。载荷为不透明 JSON（Rust 不解释字段结构）。

use serde_json::Value;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

/// 广播事件名（Rust → 全部窗口，含发起窗口；接收方按整体快照幂等应用）。
const EVENT: &str = "open-file-context-changed";

/// 宿主持有的当前上下文（整体替换；初始 Null = 尚未播种，前端忽略并等广播）。
#[derive(Default)]
pub struct OpenContextState(Mutex<Value>);

/// 读取当前上下文快照（撕裂窗口 boot 基线；快照晚于任何先前广播，二者任意到达序都收敛）。
#[tauri::command]
pub fn get_open_file_context(state: State<'_, OpenContextState>) -> Result<Value, String> {
    Ok(state
        .0
        .lock()
        .map_err(|e| format!("打开文件上下文读锁不可用：{e}"))?
        .clone())
}

/// 写入上下文（主窗口唯一写者）：锁内存入真源后向全部窗口广播（先更新后广播，保证收到
/// 广播的窗口再拉取必得 ≥ 广播版本的快照）。广播失败只记日志：撕裂窗口仍有下次广播与
/// 重开窗口兜底，不阻塞主窗口的工作区操作。
#[tauri::command]
pub fn set_open_file_context(
    app: AppHandle,
    state: State<'_, OpenContextState>,
    context: Value,
) -> Result<(), String> {
    *state
        .0
        .lock()
        .map_err(|e| format!("打开文件上下文写锁不可用：{e}"))? = context.clone();
    if let Err(e) = app.emit(EVENT, &context) {
        eprintln!("[open-context] 广播打开文件上下文失败：{e}");
    }
    Ok(())
}
