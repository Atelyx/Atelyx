//! AI 表格 / 画布编辑出口：补丁按稳定 id 合并，打开会话先合入再走保存链。

use tokio::sync::oneshot;

use atelyx_core::ai::edit_host::{CanvasOp, EditActor, TableOp};

use crate::{App, Message};

/// 应用一次表格补丁（打开会话先合入；本地写盘 / 空间补丁端点 + 历史）。
pub fn apply_table(
    app: &mut App,
    path: String,
    ops: Vec<TableOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let _ = (app, path, ops, actor);
    let _ = reply.send(Err("表格 AI 编辑尚未实现".to_string()));
    iced::Task::none()
}

/// 应用一次画布补丁（打开会话先合入；本地写盘 / 空间补丁端点 + 历史）。
pub fn apply_canvas(
    app: &mut App,
    path: String,
    ops: Vec<CanvasOp>,
    actor: EditActor,
    reply: oneshot::Sender<Result<String, String>>,
) -> iced::Task<Message> {
    let _ = (app, path, ops, actor);
    let _ = reply.send(Err("画布 AI 编辑尚未实现".to_string()));
    iced::Task::none()
}
