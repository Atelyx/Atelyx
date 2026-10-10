//! 命令适配层：仓库内文件检索（glob / grep，AI 工具后端）实现在 atelyx-core，
//! 此处取会话参数并把阻塞扫描投进阻塞线程池。
use tauri::State;

use crate::vault::VaultState;

/// 按 glob 模式枚举仓库内文件路径（相对仓库根、`/` 分隔；只返回文件；跳过隐藏目录/排除文件夹）。
#[tauri::command]
pub async fn glob_vault(
    pattern: String,
    path: Option<String>,
    state: State<'_, VaultState>,
) -> Result<atelyx_core::filesearch::GlobVaultResult, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    tauri::async_runtime::spawn_blocking(move || {
        atelyx_core::filesearch::glob_vault_blocking(root, exclude, pattern, path)
    })
    .await
    .map_err(|e| format!("检索线程失败：{e}"))?
}

/// 用正则搜索仓库内文件内容（相对仓库根路径，与 read_file 同坐标）。
#[tauri::command]
pub async fn grep_vault(
    pattern: String,
    path: Option<String>,
    include: Option<String>,
    state: State<'_, VaultState>,
) -> Result<atelyx_core::filesearch::GrepVaultResult, String> {
    let root = state.root()?;
    let exclude = state.exclude_folders()?;
    tauri::async_runtime::spawn_blocking(move || {
        atelyx_core::filesearch::grep_vault_blocking(root, exclude, pattern, path, include)
    })
    .await
    .map_err(|e| format!("检索线程失败：{e}"))?
}
