//! 仓库文件监听。
//!
//! notify 6 + notify-debouncer-mini **递归监听整个仓库根**（仓库为自由文件夹结构，
//! 不再只监听 画布/笔记/附件 三目录）。事件 debounce 300ms 后 emit `"vault-file-changed"` 到前端。
//! 仓库切换时整体替换 debouncer（drop 旧的 → new 新的），由 `commands/vault.rs` 的
//! `open_vault` 在设仓库根后调用 `start`。
//!
//! 递归监听 + 路径段过滤（隐藏 `.` 开头 / 排除文件夹）保证：
//! - 仓库内任意路径的移动/增删/编辑都能实时推送（修复旧版「外部编辑器移动笔记不刷新」——
//!   旧版只监听三目录且按前缀分类，仓库根/其他文件夹的移动事件无法正确命中）；
//! - `.atelyx/` 等隐藏目录与用户排除目录不产生事件（自写 config.json 等无回环）——
//!   例外：`.atelyx/对话历史/*.jsonl` 与 `*.meta.json`（AI 对话历史）放行，多设备共享文件夹实时互见。
//!
//! 不做智能重命名匹配：外部重命名按「旧文件删除 + 新文件创建」降解，
//! 旧引用由前端 `canvasStore.markFileMissing` 标「文件缺失」。
//!
//! **自写回波抑制契约（易回归点）**：本端对 `.atlx`/`.atb`/`.md` 的保存/重命名（原子写
//! `path.tmp` → rename）同样会触发事件——后端**不做**自写回放抑制（除 `.tmp` 副产物过滤外），
//! 抑制完全由前端承担：前端 `utils/selfSave.ts` 的 `markSelfSave`/`isSelfSaveEcho`
//! （写盘后 2s 窗口内同路径事件视为自写回波跳过）。改动本文件的事件过滤时须同步核对
//! 前端抑制逻辑，防自写事件引发「已被外部修改」误提示。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify_debouncer_mini::{new_debouncer, DebouncedEvent, Debouncer};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::vault::{is_excluded_rel, CHAT_HISTORY_DIR, CHAT_MESSAGE_EXT, CHAT_META_EXT};

/// 持有当前仓库的 debouncer；切仓库时整体替换（drop 旧的后存新的）。
/// `Debouncer<W>` 在 `W: Send` 时 `Send`，`RecommendedWatcher` 各平台均 Send，可入 Mutex。
pub struct WatcherState(pub Mutex<Option<Debouncer<notify::RecommendedWatcher>>>);

impl Default for WatcherState {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

/// emit 到前端的事件 payload（camelCase 对齐前端 `types/watcher.ts`）。
///
/// 不带 action 字段：`notify-debouncer-mini` 的 `DebouncedEventKind` 只区分 `Any`/`AnyContinuous`，
/// 不保留 Create/Modify/Remove 语义。前端用「尝试重读，失败即降级」策略——`refreshTextContent`/
/// `refreshMediaContent` 内部 try/catch，读失败 → `markFileMissing`，天然覆盖删除场景。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VaultFileChange {
    /// `"note"` | `"attachment"` | `"canvas"` | `"table"` | `"chat"`，由扩展名判定（`.md` / 其余 / `.atlx` / `.atb` / `.atelyx/对话历史/*.jsonl|*.meta.json`）
    pub kind: &'static str,
    /// 相对仓库根路径，如 `"笔记/foo.md"`（Windows 分隔符已统一为 `/`）
    pub path: String,
}

/// 启动 watcher：**先建新 debouncer 并成功 watch 仓库根（Recursive），再替换旧句柄**。
/// 任一步失败返回 Err 且旧监听保持（不会出现「state 已切新仓库但监听已销毁」的半状态）。
///
/// `app` 用于 emit 事件 + 读 `WatcherState`；`root` 为当前仓库根绝对路径；
/// `exclude` 为排除文件夹名列表（与文件树/`is_excluded_rel` 同源，隐藏 `.` 开头目录一并过滤）。
pub fn start(app: AppHandle, root: PathBuf, exclude: Vec<String>) -> Result<(), String> {
    // 1. 建新 debouncer；闭包 move AppHandle + root 副本（回调在 notify 后台线程触发）
    let app_for_cb = app.clone();
    let root_for_cb = root.clone();
    let exclude_for_cb = exclude.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(300),
        move |res: Result<Vec<DebouncedEvent>, notify::Error>| {
            if let Ok(events) = res {
                for evt in events {
                    dispatch(&app_for_cb, &root_for_cb, &exclude_for_cb, evt);
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;

    // 2. 递归监听整个仓库根（仓库自由文件夹结构；`.atelyx`/隐藏/排除目录由 dispatch 过滤；
    //    root 由调用方保证存在——open_vault 校验过目录）
    debouncer
        .watcher()
        .watch(&root, notify::RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;

    // 3. 全部 watch 成功后再替换旧句柄（失败路径下旧监听未被触碰）
    let watcher_state = app.state::<WatcherState>();
    let mut guard = watcher_state.0.lock().map_err(|e| e.to_string())?;
    *guard = Some(debouncer);
    Ok(())
}

/// 单事件分发：过滤噪声 → 相对化路径 → 分类 → emit。
fn dispatch(app: &AppHandle, root: &Path, exclude: &[String], evt: DebouncedEvent) {
    let path = &evt.path;
    // 跳过 atomic_write 的中间 `.tmp` 文件（vault.rs 原子写 `path.tmp` → rename 的副产物）
    if path.extension().and_then(|s| s.to_str()) == Some("tmp") {
        return;
    }
    // 跳过目录事件（外部新建子目录等；目录删除事件旧路径已不存在、is_dir=false 会通过，
    // 无扩展名分类为 attachment，前端仅刷新文件树，无害）
    if path.is_dir() {
        return;
    }
    // 相对化 + Windows 分隔符统一为 `/`
    let rel = match path.strip_prefix(root) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => return,
    };
    // AI 对话历史放行：`.atelyx/对话历史/*.jsonl` 与 `*.meta.json`（多设备共享文件夹实时互见）。
    // 其余 `.atelyx/` 隐藏内容（config/agents 等）仍走下方过滤，防自写回环。
    if rel.starts_with(&format!("{}/", CHAT_HISTORY_DIR))
        && (rel.ends_with(CHAT_MESSAGE_EXT) || rel.ends_with(CHAT_META_EXT))
    {
        let _ = app.emit(
            "vault-file-changed",
            VaultFileChange { kind: "chat", path: rel },
        );
        return;
    }
    // 过滤隐藏目录（`.atelyx`/`.git`/`.obsidian` 等）与用户排除文件夹（含自写配置回环抑制）
    if is_excluded_rel(&rel, exclude) {
        return;
    }
    // 分类按扩展名（文件任意文件夹存放；kind 枚举值稳定）。
    // `.canvas` 外部白板格式与 `.atlx` 同归 "canvas"：只读画布按路径匹配 → 外部修改自动重载
    let kind = match path.extension().and_then(|s| s.to_str()) {
        Some("atlx") | Some("canvas") => "canvas",
        Some("md") => "note",
        Some("atb") => "table",
        _ => "attachment",
    };
    let _ = app.emit(
        "vault-file-changed",
        VaultFileChange { kind, path: rel },
    );
}
