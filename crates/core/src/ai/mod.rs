//! AI 对话域：供应商（provider）、会话真源（session）、agent 循环（agent）、
//! 流式请求（stream）、工具协议（tools）。会话落盘格式与 `对话历史/` 路径由
//! `crate::vault` 定义，本目录内模块只经其函数读写。

pub mod agent;
pub mod canvas_refs;
pub mod doc_edit;
pub mod doc_ops;
pub mod note_edit;
pub mod edit_host;

/// 参数生成中的合成工具步 id 前缀（流式占位行；收敛时 running 态归一为已中断）。
pub const PENDING_RUN_ID_PREFIX: &str = "pending:";
pub mod provider;
pub mod refs;
pub mod session;
pub mod session_io;
pub mod stream;
pub mod table_field;

/// 单元格生成复用的 HTTP 客户端句柄（同一批次共享连接池）。
pub type AiClient = reqwest::Client;
pub mod tools;
