//! 常驻运行时的命令面：会话建立、收发与卸载（`ctx.rpc.attach` 的后端）。
//! 运行时进程与 supervisor 脚本随应用分发（资源目录，dev 回退源码目录）；首次会话惰性启动，此后常驻。

use std::sync::Arc;

use tauri::State;

use crate::host_runtime::{HostRuntimeState, HostSession};
use crate::plugin_process::PluginProcessHost;

/// 捆绑 Node 的资源子目录（与前端 `bundledRuntime`、`sync-node-runtime.mjs` 同一落点约定）。
const RUNTIME_RESOURCE_DIR: &str = "resources/runtime";
/// supervisor 脚本的资源路径（随应用分发的小文件，入库维护）。
const SUPERVISOR_RESOURCE: &str = "resources/host-runtime.mjs";

/// 解析运行时与 supervisor 脚本：资源目录优先，调试构建回退源码目录（`tauri dev` 不把资源
/// 放到二进制旁，与打包器解析同款回退）。
fn resolve_runtime_paths(app: &tauri::AppHandle) -> Result<(String, String), String> {
    use tauri::Manager;
    let binary = if cfg!(windows) { "node.exe" } else { "node" };
    let mut tried: Vec<String> = Vec::new();
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir);
    }
    if cfg!(debug_assertions) {
        candidates.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    }
    for base in candidates {
        let node = base.join(RUNTIME_RESOURCE_DIR).join(binary);
        let script = base.join(SUPERVISOR_RESOURCE);
        if node.is_file() && script.is_file() {
            return Ok((
                node.to_string_lossy().into_owned(),
                script.to_string_lossy().into_owned(),
            ));
        }
        tried.push(format!("{} / {}", node.display(), script.display()));
    }
    Err(format!(
        "常驻运行时不可用（未随应用分发）：已查找 {}",
        tried.join("；")
    ))
}

/// 建立会话（`ctx.rpc.attach` 的后端）：确保运行时进程在跑，加载插件宿主半模块并完成归属登记。
/// 模块加载在时限内不到位即失败（不让命令悬挂）；归属窗口 = 发起调用的窗口。
#[tauri::command(async)]
pub async fn host_runtime_attach(
    state: State<'_, Arc<HostRuntimeState>>,
    process_host: State<'_, Arc<PluginProcessHost>>,
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    plugin_id: String,
    module: String,
    args: Option<serde_json::Value>,
) -> Result<HostSession, String> {
    let state = state.inner().clone();
    let process_host = process_host.inner().clone();
    let label = window.label().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let (node, script) = resolve_runtime_paths(&app)?;
        state.attach(&process_host, &label, &plugin_id, &module, args, &node, &script)
    })
    .await
    .map_err(|e| format!("常驻运行时会话任务失败：{e}"))?
}

/// 上行一帧到指定会话（前端通道的 write 后端）。
#[tauri::command(async)]
pub fn host_runtime_send(
    state: State<'_, Arc<HostRuntimeState>>,
    process_host: State<'_, Arc<PluginProcessHost>>,
    session_id: u64,
    frame: String,
) -> Result<(), String> {
    state.send(&process_host, session_id, &frame)
}

/// 卸载会话（前端通道 close / 插件停用收尾的后端）：路由摘除 + 运行时侧结束模块。幂等。
#[tauri::command(async)]
pub fn host_runtime_detach(
    state: State<'_, Arc<HostRuntimeState>>,
    process_host: State<'_, Arc<PluginProcessHost>>,
    session_id: u64,
) -> Result<(), String> {
    state.detach(&process_host, session_id)
}
