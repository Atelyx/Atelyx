//! 会话容器真源命令：窗口与 Rust 真源之间的 invoke 缝。
//! 仅做参数收口与窗口 label 校验，领域行为见 crate::chat_container。

use std::sync::Arc;

use tauri::{State, WebviewWindow};

use crate::chat_container::{
    ApplyOk, ChatContainerState, ChatOp, CommitBatch, ContainerSnapshot, Fragments, IntentResult,
    EXECUTOR_LABEL,
};
use crate::vault::VaultState;

/// 拉取容器基线快照（镜像 boot / 超时重拉；窗口按 seq 单调比对增量）。
#[tauri::command]
pub fn chat_container_snapshot(state: State<'_, Arc<ChatContainerState>>) -> ContainerSnapshot {
    state.inner().snapshot()
}

/// 装载真源（进仓/切仓调用）：根取自当前激活仓库；同根幂等，换根先落盘旧仓库在途写再重建。
#[tauri::command]
pub fn chat_container_load(
    state: State<'_, Arc<ChatContainerState>>,
    vault: State<'_, VaultState>,
) -> Result<ContainerSnapshot, String> {
    let root = vault.root()?;
    state.inner().load(&root)
}

/// 容器 op 统一入口：纯持久 op 真源直应用；执行 op 转发主窗口并等待结果（无超时）。
#[tauri::command]
pub async fn chat_container_apply(
    state: State<'_, Arc<ChatContainerState>>,
    request_id: String,
    expected_root: String,
    op: ChatOp,
) -> Result<ApplyOk, String> {
    state.inner().apply(request_id, expected_root, op).await
}

/// 执行体提交编排变更：一次应用 + 差分广播；非主窗口拒绝。
#[tauri::command]
pub fn chat_container_commit(
    state: State<'_, Arc<ChatContainerState>>,
    window: WebviewWindow,
    request_id: String,
    expected_root: String,
    batch: CommitBatch,
) -> Result<Fragments, String> {
    state
        .inner()
        .commit(&request_id, window.label(), &expected_root, batch)
}

/// 执行体回填意图结果（intentId 定位挂起意图）；非主窗口拒绝。
#[tauri::command]
pub fn chat_container_intent_result(
    state: State<'_, Arc<ChatContainerState>>,
    window: WebviewWindow,
    intent_id: String,
    result: IntentResult,
) -> Result<(), String> {
    if window.label() != EXECUTOR_LABEL {
        return Err("仅主窗口可回填意图结果".to_string());
    }
    state
        .inner()
        .intent_result(&intent_id, result, window.label())
}

/// 执行体（重）启动登记：失败全部挂起意图并复位执行态；非主窗口拒绝。
#[tauri::command]
pub fn chat_container_executor_boot(
    state: State<'_, Arc<ChatContainerState>>,
    window: WebviewWindow,
) -> Result<(), String> {
    state.inner().executor_boot(window.label())
}

/// 请求立即写盘并等待本轮完成（resolve 不代表全部写成功；失败可见 + 退避重试）。
#[tauri::command]
pub async fn chat_container_flush(
    state: State<'_, Arc<ChatContainerState>>,
) -> Result<(), String> {
    state.inner().flush().await
}
