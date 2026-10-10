//! 本地仓库内容写盘广播：写命令成功后向全部窗口投递变更清单，接收窗口据此把打开中的
//! 文件与磁盘对账（前端 `stores/contentWriteBridge`，发起窗口按 `origin` 自跳过）。
//!
//! 只覆盖本地仓库：协作空间内容真源在服务端，其他窗口的变化经协作通道到达。
//! 不广播删除：外部删除本就「在打开/重读时表现」，陈旧窗口的下次写盘重建文件与既有口径一致。

use serde::Serialize;
use tauri::Emitter;

/// 跨窗口内容变更事件名（前端监听同名事件）。
const EVENT: &str = "vault-content-changed";

/// 一条内容变更（相对仓库根路径）。
#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum ContentChange {
    /// 内容落盘。`from` = Some 时路径同时漂移（title 改名），接收方先跟路径再对账。
    Write { file: String, from: Option<String> },
    /// 路径迁移（改名/移动，含目录）；文件内 title 与引用已由迁移方改写。
    Rename { from: String, file: String },
}

impl ContentChange {
    pub(crate) fn write(file: impl Into<String>) -> Self {
        Self::Write { file: file.into(), from: None }
    }

    pub(crate) fn write_drifted(file: impl Into<String>, from: impl Into<String>) -> Self {
        Self::Write { file: file.into(), from: Some(from.into()) }
    }

    pub(crate) fn rename(from: impl Into<String>, file: impl Into<String>) -> Self {
        Self::Rename { from: from.into(), file: file.into() }
    }
}

/// 广播变更给全部窗口（载荷带发起窗口 label，仓库根供接收方核对激活仓库身份防串仓）。
/// 广播失败只记日志：对账是尽力而为的实时性增强，不阻塞发起方的写盘结果。
pub(crate) fn broadcast_content_changes(
    window: &tauri::WebviewWindow,
    root: &str,
    changes: Vec<ContentChange>,
) {
    if changes.is_empty() {
        return;
    }
    let payload = serde_json::json!({ "root": root, "origin": window.label(), "changes": changes });
    if let Err(e) = window.emit(EVENT, &payload) {
        eprintln!("[content-broadcast] 广播内容变更失败：{e}");
    }
}
